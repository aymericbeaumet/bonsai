use std::fmt::Arguments;
use std::io::{self, Write};

use anyhow::Result;

pub fn write(args: Arguments<'_>) -> Result<()> {
    let mut stdout = io::stdout().lock();
    stdout.write_fmt(args)?;
    stdout.flush()?;
    Ok(())
}

pub fn line(args: Arguments<'_>) -> Result<()> {
    write(format_args!("{args}\n"))
}

pub fn is_broken_pipe(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<io::Error>()
            .is_some_and(|error| error.kind() == io::ErrorKind::BrokenPipe)
    })
}
