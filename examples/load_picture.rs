use std::{collections::HashMap, time::Duration};

use log::info;
use sci::{
    graphics::Graphics,
    picture::PictureMaps,
    resource::{self, Resource, ResourceType},
    Game,
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

        if resource::get_resource(resources, ResourceType::Pic, result).is_some() {
            info!("Navigating to picture {}", result);
            return result;
        }
    }
}

pub fn run(game: &Game) -> Result<(), String> {
    let sdl_context = sdl2::init().expect("Unable to get SDL context");

    let mut graphics = Graphics::init(&sdl_context);

    let resources = &game.resources;

    let mut resource_number = find_resource(resources, 0, true);
    let mut maps = draw_resource(&mut graphics, resources, resource_number);

    let mut event_pump = sdl_context.event_pump()?;
    'running: loop {
        graphics.clear();
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
                    keycode: Some(Keycode::V),
                    ..
                } => {
                    maps = draw_resource(&mut graphics, resources, resource_number);
                }
                Event::KeyDown {
                    keycode: Some(Keycode::P),
                    ..
                } => {
                    graphics.draw_map(&maps.priority);
                }
                Event::KeyDown {
                    keycode: Some(Keycode::C),
                    ..
                } => {
                    graphics.draw_map(&maps.control);
                }
                Event::KeyDown {
                    keycode: Some(Keycode::Left),
                    ..
                } => {
                    resource_number = find_resource(resources, resource_number, false);
                    maps = draw_resource(&mut graphics, resources, resource_number);
                }
                Event::KeyDown {
                    keycode: Some(Keycode::Right),
                    ..
                } => {
                    resource_number = find_resource(resources, resource_number, true);
                    maps = draw_resource(&mut graphics, resources, resource_number);
                }
                _ => {}
            }
        }
        ::std::thread::sleep(Duration::new(0, 1_000_000_000u32 / 60));
    }

    Ok(())
}

fn draw_resource(
    graphics: &mut Graphics,
    resources: &HashMap<u16, Resource>,
    resource_number: u16,
) -> PictureMaps {
    let resource = resource::get_resource(resources, ResourceType::Pic, resource_number).unwrap();

    graphics.clear();

    let maps = graphics.draw_picture(resource);

    graphics.present();

    maps
}
