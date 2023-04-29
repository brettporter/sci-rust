use std::path::Path;

#[macro_use]
extern crate num_derive;

mod resource;

pub struct Game<'a> {
    game_path: &'a Path,
    resource_map: Vec<resource::ResourceMapEntry>,
}

impl Game<'_> {
    pub fn load(path: &str) -> Result<Game, std::io::Error> {
        let game_path = Path::new(path);

        // TODO: possible error handling if it doesn't exist

        let resource_map = resource::read_resource_map(&game_path.join("RESOURCE.MAP").as_path())?;

        let game = Game {
            game_path,
            resource_map,
        };

        Ok(game)
    }

    pub fn run(self: Self) {
        // TODO: move specifics to examples directory instead

        // TODO: use the resource library to
        // load resources from a map entry
        // Render an image from the game to start with
    }
}
