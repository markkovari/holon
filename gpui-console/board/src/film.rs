use std::fs::File;
use std::io::BufWriter;
use std::time::Instant;

pub fn half(rgba: &[u8], w: usize, h: usize) -> (Vec<u8>, usize, usize) {
    let (hw, hh) = (w / 2, h / 2);
    let mut out = Vec::with_capacity(hw * hh * 4);
    for y in 0..hh {
        for x in 0..hw {
            for c in 0..4 {
                let at =
                    |dx: usize, dy: usize| rgba[((2 * y + dy) * w + 2 * x + dx) * 4 + c] as u16;
                out.push(((at(0, 0) + at(1, 0) + at(0, 1) + at(1, 1) + 2) / 4) as u8);
            }
        }
    }
    (out, hw, hh)
}

pub fn changed(
    prev: &[u8],
    cur: &[u8],
    w: usize,
    h: usize,
) -> Option<(usize, usize, usize, usize)> {
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for y in 0..h {
        for x in 0..w {
            let o = (y * w + x) * 4;
            if prev[o..o + 4] != cur[o..o + 4] {
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
            }
        }
    }
    (x1 > x0).then(|| (x0, y0, x1 - x0, y1 - y0))
}

pub struct Gif {
    enc: gif::Encoder<BufWriter<File>>,
    w: usize,
    h: usize,
    prev: Option<Vec<u8>>,
    pending: Option<(gif::Frame<'static>, Instant)>,
}

impl Gif {
    pub fn create(path: &str, w: usize, h: usize) -> Result<Self, String> {
        let file = File::create(path).map_err(|e| format!("{path}: {e}"))?;
        let mut enc = gif::Encoder::new(BufWriter::new(file), w as u16, h as u16, &[])
            .map_err(|e| e.to_string())?;
        enc.set_repeat(gif::Repeat::Infinite).map_err(|e| e.to_string())?;
        Ok(Self { enc, w, h, prev: None, pending: None })
    }

    pub fn push(&mut self, rgba: Vec<u8>, at: Instant) -> Result<(), String> {
        if rgba.len() != self.w * self.h * 4 {
            return Err(format!("a frame of {} bytes is not {}x{}", rgba.len(), self.w, self.h));
        }
        let (x, y, w, h) = match &self.prev {
            None => (0, 0, self.w, self.h),
            Some(p) => match changed(p, &rgba, self.w, self.h) {
                Some(r) => r,
                None => return Ok(()),
            },
        };
        let mut part = Vec::with_capacity(w * h * 4);
        for row in y..y + h {
            for col in x..x + w {
                let o = (row * self.w + col) * 4;
                let same = self.prev.as_ref().is_some_and(|p| p[o..o + 4] == rgba[o..o + 4]);
                part.extend_from_slice(if same { &[0, 0, 0, 0] } else { &rgba[o..o + 4] });
            }
        }
        let mut frame = gif::Frame::from_rgba_speed(w as u16, h as u16, &mut part, 10);
        (frame.left, frame.top) = (x as u16, y as u16);
        frame.dispose = gif::DisposalMethod::Keep;
        self.flush(at)?;
        self.pending = Some((frame, at));
        self.prev = Some(rgba);
        Ok(())
    }

    fn flush(&mut self, at: Instant) -> Result<(), String> {
        if let Some((mut f, since)) = self.pending.take() {
            // GIF delays are hundredths; players stretch anything under two
            let cs = (at.duration_since(since).as_millis() + 5) / 10;
            f.delay = cs.clamp(2, u16::MAX as u128) as u16;
            self.enc.write_frame(&f).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn finish(mut self, at: Instant) -> Result<(), String> {
        self.flush(at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_half_image_is_the_mean_of_each_square_of_four() {
        let px = [[200, 200, 200, 255], [0, 0, 0, 255], [0, 0, 0, 255], [0, 0, 0, 255]].concat();
        assert_eq!(half(&px, 2, 2), (vec![50, 50, 50, 255], 1, 1));
    }

    #[test]
    fn only_the_rectangle_that_changed_is_kept() {
        let (w, h) = (4, 3);
        let a = vec![0u8; w * h * 4];
        assert_eq!(changed(&a, &a, w, h), None, "nothing moved");
        let mut b = a.clone();
        b[(w + 1) * 4] = 9; // (1, 1)
        b[(2 * w + 2) * 4 + 2] = 9; // (2, 2)
        assert_eq!(changed(&a, &b, w, h), Some((1, 1, 2, 2)));
    }

    #[test]
    fn a_film_holds_a_still_frame_up_and_writes_a_readable_gif() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.gif");
        let t0 = Instant::now();
        let ms = |n| t0 + std::time::Duration::from_millis(n);
        let mut g = Gif::create(path.to_str().unwrap(), 2, 2).unwrap();
        let grey = vec![90u8; 16];
        let mut lit = grey.clone();
        lit[0..4].copy_from_slice(&[250, 10, 10, 255]);
        g.push(grey.clone(), ms(0)).unwrap();
        g.push(grey, ms(100)).unwrap(); // the same: not a frame
        g.push(lit, ms(300)).unwrap();
        g.finish(ms(400)).unwrap();

        let mut dec = gif::DecodeOptions::new().read_info(File::open(&path).unwrap()).unwrap();
        let first = dec.read_next_frame().unwrap().unwrap();
        assert_eq!((first.width, first.delay), (2, 30), "up until the change, 300ms");
        let second = dec.read_next_frame().unwrap().unwrap().clone();
        assert_eq!((second.left, second.top, second.width, second.height), (0, 0, 1, 1));
        assert_eq!(second.delay, 10);
        assert!(dec.read_next_frame().unwrap().is_none());
    }
}
