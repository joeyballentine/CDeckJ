//! Living on the Steam Deck: starting when the controller is plugged in, the menu entry and
//! Steam artwork, the Deck's own buttons, and keeping the screen awake.

pub mod autolaunch;
pub mod desktop;
mod gamepad;
mod keep_awake;

pub use gamepad::*;
pub use keep_awake::*;
