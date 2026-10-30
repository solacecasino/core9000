//! Hands finished pixels to the window.
//!
//! On macOS the window shows shared buffers (IOSurfaces) that the app
//! draws into directly: no extra copy in the app and none in the window
//! server. Elsewhere softbuffer does the job, which on Windows and X11 also
//! keeps a single buffer.

use std::sync::Arc;

use winit::window::Window;

use super::raster::{IRect, Target};

pub struct Presenter {
    #[cfg(target_os = "macos")]
    inner: mac::Surface,
    #[cfg(not(target_os = "macos"))]
    inner: other::Surface,
}

impl Presenter {
    pub fn new(window: Arc<Window>) -> Result<Self, String> {
        #[cfg(target_os = "macos")]
        let inner = mac::Surface::new(&window)?;
        #[cfg(not(target_os = "macos"))]
        let inner = other::Surface::new(window)?;
        Ok(Self { inner })
    }

    /// Draws a frame of `width` x `height` pixels with `paint`, which gets the
    /// pixels and how many frames ago they were drawn (0: never) and returns
    /// what it changed. Nothing is sent to the screen when nothing changed.
    pub fn draw(
        &mut self,
        window: &Window,
        width: u32,
        height: u32,
        paint: impl FnOnce(&mut Target<'_>, u8) -> Vec<IRect>,
    ) -> Result<(), String> {
        if width == 0 || height == 0 {
            return Ok(());
        }
        self.inner.draw(window, width, height, paint)
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::c_void;

    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, Bool};
    use objc2_core_foundation::CGPoint;
    use objc2_foundation::{NSDictionary, NSNumber, NSString};
    use objc2_quartz_core::{CALayer, CATransaction};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use winit::window::Window;

    use super::{IRect, Target};

    #[link(name = "IOSurface", kind = "framework")]
    unsafe extern "C" {
        fn IOSurfaceCreate(properties: *const c_void) -> *mut c_void;
        fn IOSurfaceLock(surface: *mut c_void, options: u32, seed: *mut u32) -> i32;
        fn IOSurfaceUnlock(surface: *mut c_void, options: u32, seed: *mut u32) -> i32;
        fn IOSurfaceGetBaseAddress(surface: *mut c_void) -> *mut c_void;
        fn IOSurfaceGetBytesPerRow(surface: *mut c_void) -> usize;
        fn IOSurfaceIsInUse(surface: *mut c_void) -> u8;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(object: *const c_void);
    }

    /// How many IOSurfaces take turns.
    const BUFFERS: usize = 3;

    /// IOSurfaces that take turns: one on screen while another is drawn.
    /// A buffer is only drawn into once the window server is done with it
    /// (`IOSurfaceIsInUse`): it can still be reading one that has left the
    /// screen, and drawing into that shows a frame half drawn. Two weren't
    /// enough when frames came faster than the screen shows them (moving a
    /// zoomed photo around), hence three; if all are busy, the frame waits.
    pub struct Surface {
        root: Retained<CALayer>,
        layer: Retained<CALayer>,
        surfaces: [*mut c_void; BUFFERS],
        /// When each was last put on screen, counted in frames shown; 0:
        /// never.
        shown: [u64; BUFFERS],
        /// Frames shown so far.
        serial: u64,
        /// The one on screen.
        front: usize,
        size: (u32, u32),
    }

    impl Drop for Surface {
        fn drop(&mut self) {
            self.layer.removeFromSuperlayer();
            self.release();
        }
    }

    impl Surface {
        pub fn new(window: &Window) -> Result<Self, String> {
            let handle = window.window_handle().map_err(|e| e.to_string())?;
            let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
                return Err("not an AppKit window".into());
            };
            // SAFETY: winit's handle points at the window's live NSView.
            let view: &AnyObject = unsafe { handle.ns_view.cast().as_ref() };
            let root: Option<Retained<CALayer>> = unsafe {
                let _: () = msg_send![view, setWantsLayer: Bool::YES];
                msg_send![view, layer]
            };
            let root = root.ok_or("the window has no layer")?;
            let layer = CALayer::new();
            layer.setAnchorPoint(CGPoint::new(0.0, 0.0));
            // SAFETY: a constant from QuartzCore.
            layer.setContentsGravity(unsafe { objc2_quartz_core::kCAGravityTopLeft });
            root.addSublayer(&layer);
            Ok(Self {
                root,
                layer,
                surfaces: [std::ptr::null_mut(); BUFFERS],
                shown: [0; BUFFERS],
                serial: 0,
                front: 0,
                size: (0, 0),
            })
        }

        fn release(&mut self) {
            for s in &mut self.surfaces {
                if !s.is_null() {
                    // SAFETY: we own the one reference IOSurfaceCreate gave
                    // us; the layer holds its own while it shows it.
                    unsafe { CFRelease(*s) };
                    *s = std::ptr::null_mut();
                }
            }
        }

        fn create(&mut self, width: u32, height: u32) -> Result<(), String> {
            self.release();
            let keys = [
                "IOSurfaceWidth",
                "IOSurfaceHeight",
                "IOSurfaceBytesPerElement",
                "IOSurfacePixelFormat",
            ]
            .map(NSString::from_str);
            let values = [
                NSNumber::new_u32(width),
                NSNumber::new_u32(height),
                NSNumber::new_u32(4),
                NSNumber::new_u32(u32::from_be_bytes(*b"BGRA")),
            ];
            let keys: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
            let properties = NSDictionary::from_retained_objects(&keys, &values);
            for s in &mut self.surfaces {
                // SAFETY: NSDictionary is toll-free bridged with CFDictionary.
                *s = unsafe { IOSurfaceCreate(Retained::as_ptr(&properties).cast()) };
                if s.is_null() {
                    return Err(format!("couldn't make a {width}x{height} screen buffer"));
                }
            }
            self.size = (width, height);
            self.shown = [0; BUFFERS];
            Ok(())
        }

        pub fn draw(
            &mut self,
            window: &Window,
            width: u32,
            height: u32,
            paint: impl FnOnce(&mut Target<'_>, u8) -> Vec<IRect>,
        ) -> Result<(), String> {
            let new = self.size != (width, height) || self.surfaces[0].is_null();
            if new {
                self.create(width, height)?;
            }
            // The most recently shown buffer the window server has let go
            // of: the least to redraw.
            // SAFETY: the surfaces are live.
            let free = (0..BUFFERS).filter(|&i| new || (i != self.front && unsafe { IOSurfaceIsInUse(self.surfaces[i]) } == 0));
            let Some(back) = free.max_by_key(|&i| self.shown[i]) else {
                // All busy for a moment: try again next time round.
                window.request_redraw();
                return Ok(());
            };
            let age = match self.shown[back] {
                0 => 0,
                at => u8::try_from(self.serial - at + 1).unwrap_or(0),
            };
            let surface = self.surfaces[back];
            // SAFETY: the surface is live and not on screen; the pixels are
            // only touched while it's locked.
            let damage = unsafe {
                IOSurfaceLock(surface, 0, std::ptr::null_mut());
                let base = IOSurfaceGetBaseAddress(surface).cast::<u32>();
                let stride = IOSurfaceGetBytesPerRow(surface) / 4;
                let px = std::slice::from_raw_parts_mut(base, stride * height as usize);
                let mut target = Target {
                    px,
                    stride,
                    width: width as usize,
                    height: height as usize,
                };
                let damage = paint(&mut target, age);
                IOSurfaceUnlock(surface, 0, std::ptr::null_mut());
                damage
            };
            if damage.is_empty() && !new {
                return Ok(());
            }
            self.serial += 1;
            self.shown[back] = self.serial;
            self.front = back;
            CATransaction::begin();
            CATransaction::setDisableActions(true);
            if new {
                self.layer.setFrame(self.root.bounds());
                self.layer.setContentsScale(window.scale_factor());
            }
            // SAFETY: an IOSurfaceRef is a valid layer contents object.
            let _: () =
                unsafe { msg_send![&*self.layer, setContents: surface.cast::<AnyObject>()] };
            CATransaction::commit();
            Ok(())
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod other {
    use std::num::NonZeroU32;
    use std::sync::Arc;

    use winit::window::Window;

    use super::{IRect, Target};

    pub struct Surface {
        surface: softbuffer::Surface<Arc<Window>, Arc<Window>>,
        size: (u32, u32),
        /// Nothing has been shown in a visible window yet. The first frame
        /// is drawn while the window is still hidden, and Windows and X11
        /// drop what's presented to a hidden window; sending only the
        /// changed parts after that left the rest of the window blank.
        unshown: bool,
    }

    impl Surface {
        pub fn new(window: Arc<Window>) -> Result<Self, String> {
            let context = softbuffer::Context::new(window.clone()).map_err(|e| e.to_string())?;
            let surface = softbuffer::Surface::new(&context, window).map_err(|e| e.to_string())?;
            Ok(Self {
                surface,
                size: (0, 0),
                unshown: true,
            })
        }

        pub fn draw(
            &mut self,
            window: &Window,
            width: u32,
            height: u32,
            paint: impl FnOnce(&mut Target<'_>, u8) -> Vec<IRect>,
        ) -> Result<(), String> {
            let (Some(w), Some(h)) = (NonZeroU32::new(width), NonZeroU32::new(height)) else {
                return Ok(());
            };
            if self.size != (width, height) {
                self.surface.resize(w, h).map_err(|e| e.to_string())?;
                self.size = (width, height);
            }
            let mut buffer = self.surface.buffer_mut().map_err(|e| e.to_string())?;
            let age = buffer.age();
            let mut target = Target {
                px: &mut buffer,
                stride: width as usize,
                width: width as usize,
                height: height as usize,
            };
            let damage = paint(&mut target, age);
            // The whole buffer, until a frame has reached a visible window.
            let whole = self.unshown;
            if window.is_visible() != Some(false) {
                self.unshown = false;
            }
            if whole {
                return buffer.present().map_err(|e| e.to_string());
            }
            if damage.is_empty() && age != 0 {
                return Ok(());
            }
            let rects: Vec<softbuffer::Rect> = damage
                .iter()
                .filter_map(|r| {
                    Some(softbuffer::Rect {
                        x: r.x0 as u32,
                        y: r.y0 as u32,
                        width: NonZeroU32::new((r.x1 - r.x0) as u32)?,
                        height: NonZeroU32::new((r.y1 - r.y0) as u32)?,
                    })
                })
                .collect();
            if rects.is_empty() {
                buffer.present().map_err(|e| e.to_string())
            } else {
                buffer
                    .present_with_damage(&rects)
                    .map_err(|e| e.to_string())
            }
        }
    }
}
