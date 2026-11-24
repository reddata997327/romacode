//! rc-dsd — Domain → Server → Database live API tracing tools (`dsd.*`).
//!
//! The goal: romacode can SEE an api call travel. Start a tap in front of
//! the web server (`dsd.proxy_start`), attach the database monitor
//! (`dsd.db_attach` — postgres/mysql process polling over the vendored
//! std-only wire protocol, or sqlite file watching for local server
//! databases), and `dsd.watch` renders the correlated live timeline: every
//! request's entry and exit on the network WITH the queries it triggered
//! underneath — network and database side by side, live.

pub mod dbmon;
pub mod render;
pub mod session;
pub mod tap;
pub mod tools;

pub use tools::all;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_tools_exposed() {
        assert_eq!(all().len(), 4);
    }
}
