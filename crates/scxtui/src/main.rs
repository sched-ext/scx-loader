// SPDX-License-Identifier: GPL-2.0

mod app;
mod backend;
mod ui;

use anyhow::Result;

use app::App;
use backend::loader::LoaderBackend;

fn main() -> Result<()> {
    // Connect *before* entering raw mode, so connection failures print
    // as normal error messages instead of garbling the terminal.
    let backend = LoaderBackend::connect()?;
    let mut app = App::new(Box::new(backend))?;

    let terminal = ratatui::init();
    let result = app.run(terminal);
    ratatui::restore();
    result
}
