fn main() -> Result<(), Box<dyn std::error::Error>> {
    let game = sci::Game::load("game_data/CB")?;

    game.run()?;

    Ok(())
}
