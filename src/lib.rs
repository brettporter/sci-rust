use std::{collections::HashMap, path::Path};

use resource::Resource;
use resource::ResourceType;

#[macro_use]
extern crate num_derive;

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

    pub fn run(self: Self) {
        // TODO: move specifics to examples directory instead

        let _resource = resource::get_resource(&self.resources, ResourceType::Pic, 77).unwrap();

        todo!("Do something with the resource that was loaded")

        // TODO: Render an image from the game to start with
    }
}
