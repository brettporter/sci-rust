use sdl2::{event::Event, keyboard::Keycode, EventPump, Sdl};

pub(crate) struct EventManager {
    event_pump: EventPump,
}
impl EventManager {
    pub(crate) fn init(sdl_context: &Sdl) -> Self {
        EventManager {
            event_pump: sdl_context.event_pump().unwrap(),
        }
    }

    pub(crate) fn poll(&mut self) -> Option<bool> {
        // TODO: more event support but for now just indicate whether to quit or not
        if let Some(event) = self.event_pump.poll_event() {
            match event {
                Event::Quit { .. }
                | Event::KeyDown {
                    keycode: Some(Keycode::Escape),
                    ..
                } => Some(true),
                _ => Some(false),
            }
        } else {
            None
        }
    }
}
