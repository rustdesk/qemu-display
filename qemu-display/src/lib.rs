#![allow(clippy::too_many_arguments)]

pub mod util;
#[cfg(windows)]
mod win32;

mod error;
pub use error::*;

mod vm;
pub use vm::*;

mod audio;
pub use audio::*;

mod chardev;
pub use chardev::*;

mod clipboard;
pub use clipboard::*;

mod console;
pub use console::*;

mod console_listener;
pub use console_listener::*;

mod keyboard;
pub use keyboard::*;

mod mouse;
pub use mouse::*;

mod display;
pub use display::*;

#[cfg(unix)]
mod usbredir;
#[cfg(unix)]
pub use usbredir::UsbRedir;

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}
