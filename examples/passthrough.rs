use anyhow::Result;
use clap::StructOpt;
use fuse_sys::prelude::*;
use nix::sys::stat as nixstat;
use std::{
    env,
    fs::*,
    io::ErrorKind,
    os::{raw::c_void, unix::fs::*},
};

struct Passthrough {
    root: String,
}

impl Passthrough {
    fn new(root: String) -> Self {
        Self { root }
    }

    fn source(&self, relative: &str) -> String {
        format!("{}{relative}", self.root)
    }
}

impl UnthreadedFileSystem for Passthrough {
    fn access(&mut self, path: &str, mode: libc::c_int) -> Result<i32> {
        Ok(unsafe { libc::access(path.as_ptr(), mode) })
    }

    fn chmod(
        &mut self,
        path: &str,
        mode: mode_t,
        _info: Option<&mut fuse_file_info>,
    ) -> Result<i32> {
        set_permissions(self.source(path), Permissions::from_mode(mode.into()))?;
        Ok(0)
    }

    fn create(
        &mut self,
        path: &str,
        mode: mode_t,
        info: Option<&mut fuse_file_info>,
    ) -> Result<i32> {
        let mut options = OpenOptions::new();
        if let Some(info) = info {
            options.custom_flags(info.flags);
        }

        options
            .create(true)
            .append(true)
            .mode(mode.into())
            .open(self.source(path))?;

        Ok(0)
    }

    fn fsync(
        &mut self,
        _path: &str,
        _datasync: i32,
        _info: Option<&mut fuse_file_info>,
    ) -> Result<i32> {
        Ok(0)
    }

    fn getattr(
        &mut self,
        path: &str,
        stat: Option<&mut stat>,
        _info: Option<&mut fuse_file_info>,
    ) -> Result<i32> {
        let path: &str = &self.source(path);
        *stat.unwrap() = unsafe { std::mem::transmute(nixstat::stat(path)?) };
        Ok(0)
    }

    fn mkdir(&mut self, path: &str, mode: mode_t) -> Result<i32> {
        let path = self.source(path);
        create_dir(&path)?;
        set_permissions(path, Permissions::from_mode(mode.into()))?;
        Ok(0)
    }

    fn mknod(&mut self, path: &str, mode: mode_t, dev: dev_t) -> Result<i32> {
        let path: &str = &self.source(path);
        nixstat::mknod(
            path,
            nixstat::SFlag::from_bits_truncate(mode),
            nixstat::Mode::from_bits_truncate(mode),
            dev,
        )?;
        Ok(0)
    }

    fn read(
        &mut self,
        path: &str,
        buf: &mut [u8],
        off: off_t,
        info: Option<&mut fuse_file_info>,
    ) -> Result<i32> {
        let mut options = OpenOptions::new();
        if let Some(info) = info {
            options.custom_flags(info.flags);
        }

        let f = options.read(true).open(self.source(path))?;
        let n = f.read_at(buf, off as u64)?;
        Ok(n as i32)
    }

    fn readdir(
        &mut self,
        path: &str,
        buf: Option<&mut c_void>,
        filler: fuse_fill_dir_t,
        _off: off_t,
        _info: Option<&mut fuse_file_info>,
        _flags: fuse_readdir_flags,
    ) -> Result<i32> {
        let filler = filler.unwrap();

        let buf = match buf {
            Some(buf) => buf,
            None => return Ok(0),
        };

        for entry in read_dir(self.source(path))? {
            let entry = entry?;

            let stat = stat {
                st_ino: entry.ino(),
                ..Default::default()
            };

            unsafe {
                if filler(
                    buf,
                    entry.file_name().to_str().unwrap().as_ptr(),
                    &stat,
                    0,
                    0,
                ) != 0
                {
                    break;
                }
            }
        }

        Ok(0)
    }

    fn readlink(&mut self, path: &str, buf: &mut [u8]) -> Result<i32> {
        if buf.is_empty() {
            return Ok(0);
        }

        let link_buf = read_link(self.source(path))?;
        let link = link_buf.to_str().unwrap().as_bytes();

        let length = buf.len().min(link.len());
        (&mut buf[..length]).copy_from_slice(&link[..length]);

        let null = length.min(buf.len() - 1);
        buf[null] = 0;

        Ok(0)
    }

    fn rename(&mut self, old: &str, new: &str, _flags: fuse_readdir_flags) -> Result<i32> {
        rename(self.source(old), self.source(new))?;
        Ok(0)
    }

    fn rmdir(&mut self, path: &str) -> Result<i32> {
        remove_dir(self.source(path))?;
        Ok(0)
    }

    fn truncate(
        &mut self,
        path: &str,
        size: off_t,
        _info: Option<&mut fuse_file_info>,
    ) -> Result<i32> {
        let f = OpenOptions::new().write(true).open(self.source(path))?;
        f.set_len(size as u64)?;
        Ok(0)
    }

    fn unlink(&mut self, path: &str) -> Result<i32> {
        remove_file(self.source(path))?;
        Ok(0)
    }

    fn write(
        &mut self,
        path: &str,
        buf: &[u8],
        off: off_t,
        info: Option<&mut fuse_file_info>,
    ) -> Result<i32> {
        let mut options = OpenOptions::new();
        if let Some(info) = info {
            options.custom_flags(info.flags);
        }

        let n = options
            .write(true)
            .open(self.source(path))?
            .write_at(buf, off as u64)?;

        Ok(n as i32)
    }
}

#[derive(clap::Parser)]
struct Args {
    /// The path of filesystem's mount
    #[clap(short, long, default_value = "/tmp/fsmnt")]
    mount: String,
    /// The directory that backs mount
    #[clap(short = 'a', long, default_value = "/tmp/fsdata")]
    data: String,
    /// Whether or not to run fuse in debug mode
    #[clap(short, long)]
    debug: bool,
}

fn main() {
    let bin = env::args().next().unwrap();
    let Args { mount, data, debug } = Args::parse();

    let mut fuse_args = vec![bin.as_str(), mount.as_str(), "-f", "-s"];
    if debug {
        fuse_args.push("-d");
    }

    match read_dir(&mount) {
        Err(e) if e.kind() == ErrorKind::NotFound => create_dir(&mount).unwrap(),
        r => {
            r.unwrap();
        }
    }
    match read_dir(&data) {
        Err(e) if e.kind() == ErrorKind::NotFound => create_dir(&data).unwrap(),
        r => {
            r.unwrap();
        }
    }

    println!("Mounting {mount} as mirror of {data}...");
    Passthrough::new(data).run(&fuse_args).unwrap();
}
