use std::collections::VecDeque;

use sdl2::{
    pixels::Color,
    rect::{Point, Rect},
    render::Canvas,
    video::Window,
    Sdl,
};

use crate::{picture, resource::Resource};

pub(crate) struct Graphics {
    canvas: Canvas<Window>,
}
impl Graphics {
    // TODO: replace these with values from the window
    pub(crate) const VIEWPORT_WIDTH: i32 = 320;
    pub(crate) const VIEWPORT_HEIGHT: i32 = 190;

    pub(crate) fn init(sdl_context: &Sdl) -> Self {
        // TOOD: replace expect with error handling, this can certainly fail
        let video_subsystem = sdl_context.video().expect("Unable to create SDL 2 video");

        // TODO: make configurable
        let window = video_subsystem
            .window("SCI Player", 1920, 1200)
            .position_centered()
            .build()
            .expect("could not initialize video subsystem");

        let mut canvas = window
            .into_canvas()
            .build()
            .expect("could not make a canvas");

        // TODO: need to adjust coordinates to the canvas size of the game
        // Requires loading script, but this is likely to be the default for most
        canvas
            .set_logical_size(320, 200)
            .expect("Unable to set the logical size of the window canvas");

        Self { canvas }
    }

    // TODO: Consider moving this into graphics. However, maybe refactor to get the surface from graphics to draw onto, not draw image in the middle (though if we are drawing straight to our own buffer, maybe?)
    // What is the right thing to pass into draw_image since I only get the texture canvas in the loop
    // But it's not super useful since it's all point drawing
    //  -- refactor all the canvas bits in picture to something I can narrow down to a simple implementation
    pub(crate) fn render_resource(&mut self, resource: &Resource) {
        // TODO: better to just do the above with bytes and create the texture raw?
        // TODO: factor in menu bar -- currently full screen white, but should be white background for the picture viewport, black for the rest (when no menu bar)
        // TODO: don't necessarily want entire clear -> copy -> present logic here or if there are other steps for the current scene, currently an example

        let canvas = &mut self.canvas;
        canvas.set_draw_color(Color::BLACK);
        canvas.clear();

        let creator = canvas.texture_creator();
        // TODO: hardcoded dimensions defined in graphics
        let mut texture = creator
            .create_texture_target(
                canvas.default_pixel_format(),
                Self::VIEWPORT_WIDTH as u32,
                Self::VIEWPORT_HEIGHT as u32,
            )
            .unwrap();

        canvas
            .with_texture_canvas(&mut texture, |texture_canvas| {
                texture_canvas.set_draw_color(Color::WHITE);
                texture_canvas.clear();
                // TODO: avoid circular dependency
                picture::draw_image(
                    &mut GraphicsContext {
                        canvas: texture_canvas,
                    },
                    resource,
                )
            })
            .expect("Unable to render to a texture on the canvas");

        canvas
            .copy(&texture, None, Rect::new(0, 10, 320, 190))
            .expect("Unable to copy texture to the canvas");

        canvas.present();
    }
}

pub(crate) struct GraphicsContext<'a> {
    canvas: &'a mut Canvas<Window>,
}
impl<'a> GraphicsContext<'a> {
    pub(crate) fn set_draw_color(&mut self, r: u8, g: u8, b: u8) {
        self.canvas.set_draw_color(Color::RGB(r, g, b));
    }

    pub(crate) fn draw_point(&mut self, x: i32, y: i32) {
        self.canvas.draw_point(Point::new(x, y)).unwrap();
    }

    // TODO: review implementation
    pub(crate) fn flood_fill(&mut self, x: i32, y: i32) {
        // TODO: this is expensive
        // A better approach may be for picture to draw to a buffer storing the dithered colour references (one byte each)
        // so we are not reading the whole framebuffer, then draw that onto the texture by converting to colours
        let mut v = self
            .canvas
            .read_pixels(None, self.canvas.default_pixel_format())
            .unwrap();

        let mut q = VecDeque::new();
        q.push_front(Point::new(x, y));

        let pitch = self.canvas.default_pixel_format().byte_size_per_pixel();
        let w = self.canvas.viewport().w as usize;

        while !q.is_empty() {
            let p = q.pop_front().unwrap();

            let offset = (p.y as usize * w + p.x as usize) * pitch;
            // TODO: currently filling over white
            if v[offset..offset + 3] != [255, 255, 255] {
                continue;
            }

            self.canvas.draw_point(p).unwrap();
            // TODO: temporarily setting not-white so the algorithm doesn't revisit it, but won't impact the actual framebuffer
            v[offset] = 0;

            if p.x < Graphics::VIEWPORT_WIDTH - 1 {
                q.push_front(p.offset(1, 0));
            }
            if p.y < Graphics::VIEWPORT_HEIGHT - 1 {
                q.push_front(p.offset(0, 1));
            }
            if p.x > 0 {
                q.push_front(p.offset(-1, 0));
            }
            if p.y > 0 {
                q.push_front(p.offset(0, -1));
            }
        }
    }
}
