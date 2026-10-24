//! RustPy IDE — Jupyter notebook support (roadmap §6 "Jupyter notebooks").
//!
//! * [`Notebook`], [`Cell`], [`Output`]: the `.ipynb` (nbformat 4) model with
//!   [`Notebook::load`] / [`Notebook::save`], [`str::parse`] / [`ToString::to_string`],
//!   [`new_notebook`] and cell helpers ([`Notebook::insert_cell`], [`Notebook::delete_cell`],
//!   [`Notebook::move_cell`]). Everything the model does not interpret (unknown metadata, cell and
//!   output fields, unknown output types) survives a load/save cycle, and files are written the way
//!   Jupyter writes them, so an unmodified notebook round-trips byte for byte.
//! * [`Notebook::to_py`] / [`Notebook::from_py`]: conversion to and from Python scripts in the
//!   "percent" format (`# %%` cells, as used by Jupytext, VS Code and Spyder).
//! * [`CellRunner`]: runs code cells on a Python process through the `rustpy-console` backend with
//!   notebook semantics (execution counts, stdout/stderr streams, results, rich display data,
//!   errors), with run cell / all / above / below, interrupt, restart and a non-blocking
//!   [`CellRunner::poll`] reporting which cells changed.
//!
//! ```
//! use rustpy_notebook::{new_notebook, Cell, Notebook};
//!
//! let mut nb = new_notebook();
//! nb.cells[0].source = "print('hi')".into();
//! nb.insert_cell(0, Cell::markdown("# Analysis"));
//! let ipynb = nb.to_string(); // the file text, as Jupyter writes it
//! assert_eq!(ipynb.parse::<Notebook>().unwrap(), nb);
//! assert_eq!(nb.to_py(), "# %% [markdown]\n# # Analysis\n\n# %%\nprint('hi')\n");
//! ```
//!
//! Running cells (the UI calls `poll` once per frame instead of looping):
//!
//! ```no_run
//! use std::path::Path;
//! use rustpy_notebook::{CellRunner, Notebook};
//!
//! let mut nb = Notebook::load("analysis.ipynb")?;
//! let mut kernel = CellRunner::start(Path::new("python3"), Path::new("."), &[], "")?;
//! kernel.run_all(&mut nb);
//! while kernel.is_busy() {
//!     for index in kernel.poll(&mut nb) {
//!         println!("cell {index}: {:?}", nb.cells[index].outputs);
//!     }
//!     std::thread::sleep(std::time::Duration::from_millis(16));
//! }
//! nb.save("analysis.ipynb")?;
//! # Ok::<(), anyhow::Error>(())
//! ```

mod model;
mod percent;
mod runner;

pub use model::{
    new_notebook, Cell, CellKind, JsonMap, MimeBundle, Notebook, Output, NBFORMAT, NBFORMAT_MINOR,
};
pub use runner::{CellRunner, CellStatus, KernelState};
pub use rustpy_console::ConsoleEvent;

mod session;
pub use session::Connection;

pub mod virtual_file;

pub mod tasks;
