//! Headless rendering: a surfaceless EGL display, a GLES renderer and an
//! offscreen render target.
//!
//! Smithay 0.7 has no headless backend (see ADR 0005), so Inox builds this path
//! from Smithay's EGL and GLES abstractions: a surfaceless `EGLDisplay`
//! (`EGL_MESA_platform_surfaceless`), a configless `EGLContext`, a `GlesRenderer`
//! and a renderbuffer sized to the output mode.
//!
//! Since M2 the render target also composites the client windows that the
//! compositor manages: [`HeadlessRenderer::render_elements`] clears to the
//! background colour and then draws the render elements produced by the desktop
//! `Space` (see [`crate::state`]).
//!
//! # Unsafe code
//!
//! This module holds the crate's `unsafe` allowance, which is localised to
//! [`HeadlessRenderer::new`]: `EGLDisplay` and `GlesRenderer` can only be
//! constructed through `unsafe` constructors whose contracts are tied to
//! process-wide EGL/GL state. Both calls are wrapped into a safe API and carry
//! `// SAFETY:` justifications.

use smithay::{
    backend::{
        allocator::Fourcc,
        egl::{EGLContext, EGLDisplay, Error as EglError, native::EGLSurfacelessDisplay},
        renderer::{
            Bind, ExportMem, Frame, Offscreen, Renderer,
            element::RenderElement,
            gles::{GlesError, GlesRenderbuffer, GlesRenderer},
        },
    },
    utils::{Buffer as BufferCoord, Physical, Rectangle, Scale, Size, Transform},
};

/// Background colour of the headless output, as 8-bit RGBA.
pub const CLEAR_COLOR_RGBA8: [u8; 4] = [17, 34, 51, 255];

/// [`CLEAR_COLOR_RGBA8`] in the normalised floats GL expects.
pub const CLEAR_COLOR: [f32; 4] = [
    CLEAR_COLOR_RGBA8[0] as f32 / 255.0,
    CLEAR_COLOR_RGBA8[1] as f32 / 255.0,
    CLEAR_COLOR_RGBA8[2] as f32 / 255.0,
    CLEAR_COLOR_RGBA8[3] as f32 / 255.0,
];

/// Bytes per pixel of the render target, which uses [`Fourcc::Abgr8888`].
const BYTES_PER_PIXEL: usize = 4;

/// Errors produced by the headless renderer.
#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    /// The EGL display or context could not be created.
    #[error("failed to create the EGL display or context")]
    Egl(#[from] EglError),

    /// A GLES call failed.
    #[error("GLES rendering failed")]
    Gles(#[from] GlesError),

    /// The render target was read back as fewer bytes than its geometry requires.
    #[error("short readback: expected {expected} bytes, got {actual}")]
    ShortReadback {
        /// Number of bytes the readback needed to contain.
        expected: usize,
        /// Number of bytes the readback produced.
        actual: usize,
    },
}

/// An offscreen GLES render target sized to a headless output.
pub struct HeadlessRenderer {
    renderer: GlesRenderer,
    target: GlesRenderbuffer,
    size: Size<i32, Physical>,
}

impl HeadlessRenderer {
    /// Create the EGL/GLES renderer and a render target of `size` pixels.
    ///
    /// # Errors
    ///
    /// Returns a [`RenderError`] when no usable GL driver is available, or when the
    /// render target cannot be allocated.
    #[allow(unsafe_code)]
    pub fn new(size: (i32, i32)) -> Result<Self, RenderError> {
        // SAFETY: `EGLDisplay::new` takes ownership of the platform display and calls
        // `eglTerminate` when it is dropped. The compositor obtains its EGL display
        // through this single path, so there is no other owner to invalidate.
        let display = unsafe { EGLDisplay::new(EGLSurfacelessDisplay) }?;
        let context = EGLContext::new(&display)?;

        // SAFETY: `GlesRenderer::new` requires the context not to be current on any
        // other thread. The compositor is single-threaded, and this context was just
        // created here, so no other thread can be using it.
        let mut renderer = unsafe { GlesRenderer::new(context) }?;

        // `Abgr8888` is the only format the GLES renderer can allocate as a
        // renderbuffer without extra capabilities.
        let target =
            renderer.create_buffer(Fourcc::Abgr8888, Size::<i32, BufferCoord>::from(size))?;

        Ok(Self {
            renderer,
            target,
            size: size.into(),
        })
    }

    /// The underlying GLES renderer.
    ///
    /// The compositor needs it to import client buffers and produce the render
    /// elements for the desktop `Space` before calling
    /// [`HeadlessRenderer::render_elements`].
    pub fn renderer_mut(&mut self) -> &mut GlesRenderer {
        &mut self.renderer
    }

    /// Render `elements` over the background colour into the render target.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::Gles`] when any GLES call fails.
    pub fn render_elements<E>(&mut self, elements: &[E]) -> Result<(), RenderError>
    where
        E: RenderElement<GlesRenderer>,
    {
        let size = self.size;
        let damage = [Rectangle::from_size(size)];

        let mut bound = self.renderer.bind(&mut self.target)?;

        {
            let mut frame = self.renderer.render(&mut bound, size, Transform::Normal)?;
            frame.clear(CLEAR_COLOR.into(), &damage)?;

            for element in elements {
                element.draw(
                    &mut frame,
                    element.src(),
                    element.geometry(Scale::from(1.0f64)),
                    &damage,
                    &[],
                )?;
            }

            // The readback performed by callers blocks until the frame is complete, so
            // the sync point is deliberately dropped rather than waited on.
            drop(frame.finish()?);
        }

        Ok(())
    }

    /// Read back the pixel at the centre of the render target, as RGBA bytes.
    ///
    /// Integration tests use this as evidence that a frame really reached the render
    /// target. The colour is returned with the raw 8-bit precision of the target, so
    /// callers should compare with a one-step tolerance.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::Gles`] when the readback fails, and
    /// [`RenderError::ShortReadback`] when the mapping is smaller than expected.
    pub fn read_center_pixel(&mut self) -> Result<[u8; 4], RenderError> {
        let size = self.size;

        let region = Rectangle::from_size(Size::<i32, BufferCoord>::from((size.w, size.h)));

        let bound = self.renderer.bind(&mut self.target)?;
        let mapping = self
            .renderer
            .copy_framebuffer(&bound, region, Fourcc::Abgr8888)?;
        let data = self.renderer.map_texture(&mapping)?;

        // `Abgr8888` readbacks are tightly packed, so the stride is the width in bytes.
        let stride = size.w as usize * BYTES_PER_PIXEL;
        let offset = (size.h as usize / 2) * stride + (size.w as usize / 2) * BYTES_PER_PIXEL;

        data.get(offset..offset + BYTES_PER_PIXEL)
            .and_then(|bytes| <[u8; BYTES_PER_PIXEL]>::try_from(bytes).ok())
            .ok_or(RenderError::ShortReadback {
                expected: offset + BYTES_PER_PIXEL,
                actual: data.len(),
            })
    }
}
