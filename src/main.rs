fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let game = sci::Game::load("game_data/CB")?;

    game.run()?;

    Ok(())
}
