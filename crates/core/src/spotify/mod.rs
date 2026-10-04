//! What Spotify is doing: which process, which track, playing or not, ad
//! or music. `title` and `state` are pure logic; the rest reads Windows.

pub mod cli;
pub mod link;
pub mod state;
pub mod title;

#[cfg(windows)]
pub mod monitor;
#[cfg(windows)]
pub mod process;
#[cfg(windows)]
pub mod smtc;
#[cfg(windows)]
pub mod window;

#[cfg(windows)]
pub use process::find_root_pid;
