//! The emulator window as an Analog Four MKII: `rytm` with the A4 defaults
//! (fw/Analog-Four_MKII_OS1.56.syx, card a4.img); arguments are the same.

#[path = "rytm.rs"]
mod window;

fn main() {
    window::main()
}
