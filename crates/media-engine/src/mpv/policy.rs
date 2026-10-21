use super::{EngineMode, ffi::Client};
use crate::{EngineError, SelectedFile};
use tokio_util::sync::CancellationToken;

pub(super) fn apply(client: &Client, mode: EngineMode) -> Result<(), EngineError> {
    for (key, value) in [
        (c"config", c"no"),
        (c"access-references", c"no"),
        (c"autoload-files", c"no"),
        (c"load-scripts", c"no"),
        (c"ytdl", c"no"),
        (c"load-stats-overlay", c"no"),
        (c"load-console", c"no"),
        (c"load-auto-profiles", c"no"),
        (c"load-select", c"no"),
        (c"load-positioning", c"no"),
        (c"load-commands", c"no"),
        (c"input-default-bindings", c"no"),
        (c"input-vo-keyboard", c"no"),
        (c"input-terminal", c"no"),
        (c"osc", c"no"),
        (c"osd-level", c"0"),
        (c"terminal", c"no"),
        (c"msg-level", c"all=no"),
        (c"input-ipc-server", c""),
        (c"cache", c"no"),
        (c"cache-on-disk", c"no"),
        (c"sub-auto", c"no"),
        (c"audio-file-auto", c"no"),
        (c"demuxer", c"lavf"),
        (c"demuxer-lavf-o", c"format_whitelist=%12%mov,matroska"),
        (c"idle", c"yes"),
        (c"keep-open", c"yes"),
    ] {
        client
            .option(key, value)
            .map_err(|_| EngineError::Unavailable)?;
    }
    match mode {
        EngineMode::Headless => {
            client.option(c"vo", c"null")?;
            client.option(c"ao", c"null")?;
            client.option(c"hwdec", c"no")?;
        }
        EngineMode::Native => {
            client.option(c"vo", c"libmpv")?;
            client.option(c"hwdec", c"auto-copy")?;
        }
    }
    Ok(())
}
/// MOV-only options are supplied per load and never passed to an EBML file.
pub(super) fn load_options(
    file: &SelectedFile,
    cancel: &CancellationToken,
) -> Result<&'static str, EngineError> {
    let mut header = [0; 4];
    let n = file.read_at(0, &mut header, cancel)?;
    if n == 4 && header == [0x1a, 0x45, 0xdf, 0xa3] {
        Ok("format_whitelist=%12%mov,matroska")
    } else {
        Ok("format_whitelist=%12%mov,matroska,enable_drefs=0,use_absolute_path=0")
    }
}
