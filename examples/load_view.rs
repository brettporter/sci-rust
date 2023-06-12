use std::{collections::HashMap, time::Duration};

use log::info;
use sci::{
    graphics::Graphics,
    resource::{self, Resource, ResourceType},
    view, Game,
};
use sdl2::{event::Event, keyboard::Keycode};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let game = Game::load("game_data/CB")?;

    Ok(run(&game)?)
}
// Use for navigation in the main loop
fn find_resource(resources: &HashMap<u16, Resource>, resource_number: u16, inc: bool) -> u16 {
    let mut result = resource_number;
    loop {
        if inc {
            result += 1;
            if result > 999 {
                result = 0;
            }
        } else {
            result -= 1;
            if result == 0 {
                result = 999;
            }
        }

        if resource::get_resource(resources, ResourceType::View, result).is_some() {
            info!("Navigating to view {}", result);
            return result;
        }
    }
}

pub fn run(game: &Game) -> Result<(), String> {
    let sdl_context = sdl2::init().expect("Unable to get SDL context");

    let mut graphics = Graphics::init(&sdl_context);

    let resources = &game.resources;

    let mut group = 0;
    let mut cel = 0;

    let mut resource_number = find_resource(resources, 0, true);
    let resource = resource::get_resource(resources, ResourceType::View, resource_number).unwrap();
    let mut view = view::load_view(&resource);
    graphics.render_view(&view, group, cel);

    let mut event_pump = sdl_context.event_pump()?;
    'running: loop {
        for event in event_pump.poll_iter() {
            match event {
                Event::Quit { .. }
                | Event::KeyDown {
                    keycode: Some(Keycode::Escape),
                    ..
                } => {
                    break 'running;
                }
                Event::KeyDown {
                    keycode: Some(Keycode::Up),
                    ..
                } => {
                    group = if group > 0 { group - 1 } else { group };
                    cel = 0;
                    graphics.render_view(&view, group, cel);
                }
                Event::KeyDown {
                    keycode: Some(Keycode::Down),
                    ..
                } => {
                    group = if group < view.len() - 1 {
                        group + 1
                    } else {
                        group
                    };
                    cel = 0;
                    graphics.render_view(&view, group, cel);
                }
                Event::KeyDown {
                    keycode: Some(Keycode::Space),
                    ..
                } => {
                    cel += 1;
                    cel %= view[group].len();
                    graphics.render_view(&view, group, cel);
                }
                Event::KeyDown {
                    keycode: Some(Keycode::Left),
                    ..
                } => {
                    group = 0;
                    cel = 0;

                    resource_number = find_resource(resources, resource_number, false);
                    let resource =
                        resource::get_resource(resources, ResourceType::View, resource_number)
                            .unwrap();
                    view = view::load_view(&resource);
                    graphics.render_view(&view, group, cel);
                }
                Event::KeyDown {
                    keycode: Some(Keycode::Right),
                    ..
                } => {
                    group = 0;
                    cel = 0;

                    resource_number = find_resource(resources, resource_number, true);
                    let resource =
                        resource::get_resource(resources, ResourceType::View, resource_number)
                            .unwrap();
                    view = view::load_view(&resource);
                    graphics.render_view(&view, group, cel);
                }
                _ => {}
            }
        }
        ::std::thread::sleep(Duration::new(0, 1_000_000_000u32 / 60));
        // todo!("loop throw cels / loop")
    }

    Ok(())
}
