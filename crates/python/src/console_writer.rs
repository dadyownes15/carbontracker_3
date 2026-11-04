//! [PyWriter]: the console's lines into Python, so they land in a notebook
//! cell and around tqdm's bars rather than on the process's own stdout.

use std::io;

use pyo3::prelude::*;

/// Calls a Python function, `print` or `tqdm.write`, once per line. Runs on
/// the run's writer thread and takes the GIL for each line.
pub struct PyWriter {
    write: Py<PyAny>,
    /// Bytes up to the next newline.
    line: Vec<u8>,
}

impl PyWriter {
    pub fn new(write: Py<PyAny>) -> Self {
        Self { write, line: Vec::new() }
    }
}

impl io::Write for PyWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.line.extend_from_slice(bytes);
        while let Some(end) = self.line.iter().position(|&byte| byte == b'\n') {
            let line: Vec<u8> = self.line.drain(..=end).collect();
            let text = String::from_utf8_lossy(&line[..end]).into_owned();
            Python::attach(|py| self.write.call1(py, (text,))).map_err(io::Error::other)?;
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
