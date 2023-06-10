use std::{collections::HashMap, path::Path};

use events::EventManager;
use graphics::Graphics;
use pmachine::PMachine;
use resource::Resource;

#[macro_use]
extern crate num_derive;

mod events;
pub mod graphics;
mod picture;
mod pmachine;
pub mod resource;
mod script;

pub struct Game {
    pub resources: HashMap<u16, Resource>,
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

    pub fn run(&self) -> Result<(), String> {
        let sdl_context = sdl2::init().expect("Unable to get SDL context");

        let mut graphics = Graphics::init(&sdl_context);

        let mut event_manager = EventManager::init(&sdl_context);
        // TODO: initialise window manager
        // TODO: initialise text parser (vocabulary files)
        // TODO: initialise the music player

        let vm = PMachine::init(&self.resources);

        vm.run_game_play_method(&mut graphics, &mut event_manager);

        Ok(())
    }
}
