use std::time::Duration;
use std::{collections::HashMap, path::Path};

use graphics::Graphics;
use log::info;
use resource::{Resource, ResourceType};
use sdl2::event::Event;
use sdl2::keyboard::Keycode;

#[macro_use]
extern crate num_derive;

mod graphics;
mod picture;
mod resource;

pub struct Game {
    resources: HashMap<u16, Resource>,
}

impl Game {
    pub fn load(path: &str) -> Result<Game, std::io::Error> {
        let game_path = Path::new(path);

        // TODO: possible error handling if it doesn't exist

        // Load all the game resources into memory
        // If this were bigger we could load on demand into a cache but not needed for these game sizes
        // Map not needed after loading all resources but we'll keep it for this use case

        // TODO: maybe a resource library type instead of a hashmap
        let resources = resource::load_all_resources(&game_path)?;

        // Since everything is loaded, no need to hold on to game path or resource map for now
        let game = Game { resources };

        Ok(game)
    }

    // Use for navigation in the main loop
    // TODO: move to examples
    fn find_resource(&self, resource_number: u16, inc: bool) -> u16 {
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

            if resource::get_resource(&self.resources, ResourceType::Pic, result).is_some() {
                info!("Navigating to picture {}", result);
                return result;
            }
        }
    }

    pub fn run(&self) -> Result<(), String> {
        let sdl_context = sdl2::init().expect("Unable to get SDL context");

        let mut graphics = Graphics::init(&sdl_context);

        // TODO: move examples to examples directory instead
        let mut resource_number = self.find_resource(0, true);
        let resource =
            resource::get_resource(&self.resources, ResourceType::Pic, resource_number).unwrap();
        graphics.render_resource(resource);

        let _init_script_resource =
            resource::get_resource(&self.resources, ResourceType::Script, 0).unwrap();

        // TODO: set up the virtual machine and load the play method from the game object in script 000

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
                        keycode: Some(Keycode::Left),
                        ..
                    } => {
                        resource_number = self.find_resource(resource_number, false);
                        let resource = resource::get_resource(
                            &self.resources,
                            ResourceType::Pic,
                            resource_number,
                        )
                        .unwrap();
                        graphics.render_resource(resource);
                    }
                    Event::KeyDown {
                        keycode: Some(Keycode::Right),
                        ..
                    } => {
                        resource_number = self.find_resource(resource_number, true);
                        let resource = resource::get_resource(
                            &self.resources,
                            ResourceType::Pic,
                            resource_number,
                        )
                        .unwrap();
                        graphics.render_resource(resource);
                    }
                    _ => {}
                }
            }
            ::std::thread::sleep(Duration::new(0, 1_000_000_000u32 / 60));
        }

        Ok(())
    }
}
