use std::path::Path;

mod resource;

pub struct Game {}

impl Game {
    pub fn load(path: &str) -> Result<Game, std::io::Error> {
        let game_path = Path::new(path);

        // TODO: possible error handling if it doesn't exist

        let game = Game {};

        todo!("Read resource.map");

        Ok(game)
    }

    pub fn run(self: Self) {
        todo!("Render an image from the game to start with");
    }
}
