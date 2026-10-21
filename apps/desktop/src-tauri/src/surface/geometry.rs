use media_engine::EngineError;
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PhysicalSize {
    pub width: u32,
    pub height: u32,
}
#[derive(Clone, Copy, Debug)]
pub struct SurfaceGeometry {
    pub width: f64,
    pub height: f64,
    pub scale: f64,
}
impl SurfaceGeometry {
    pub fn from_logical(
        width: f64,
        height: f64,
        scale: f64,
    ) -> Result<Option<PhysicalSize>, EngineError> {
        if !width.is_finite()
            || !height.is_finite()
            || !scale.is_finite()
            || width < 0.
            || height < 0.
            || scale <= 0.
        {
            return Err(EngineError::InvalidCommand);
        }
        let w = (width * scale).round();
        let h = (height * scale).round();
        if !w.is_finite() || !h.is_finite() || w > i32::MAX as f64 || h > i32::MAX as f64 {
            return Err(EngineError::InvalidCommand);
        }
        if w == 0. || h == 0. {
            return Ok(None);
        }
        Ok(Some(PhysicalSize {
            width: w as u32,
            height: h as u32,
        }))
    }
    pub fn physical(self) -> Result<Option<PhysicalSize>, EngineError> {
        Self::from_logical(self.width, self.height, self.scale)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn surface_geometry() {
        assert_eq!(
            SurfaceGeometry::from_logical(800., 450., 1.5),
            Ok(Some(PhysicalSize {
                width: 1200,
                height: 675
            }))
        );
        assert_eq!(SurfaceGeometry::from_logical(0., 450., 1.), Ok(None));
        for (w, h, s) in [
            (f64::NAN, 1., 1.),
            (1., f64::INFINITY, 1.),
            (-1., 1., 1.),
            (1., -1., 1.),
            (1., 1., 0.),
            (1., 1., -1.),
            (1., 1., f64::NAN),
            (1., 1., f64::INFINITY),
            (u32::MAX as f64, 1., 2.),
            (i32::MAX as f64 + 1., 1., 1.),
        ] {
            assert_eq!(
                SurfaceGeometry::from_logical(w, h, s),
                Err(EngineError::InvalidCommand)
            );
        }
        assert_eq!(
            SurfaceGeometry::from_logical(800., 450., 1.)
                .unwrap()
                .unwrap()
                .width,
            800
        );
        assert_eq!(
            SurfaceGeometry::from_logical(800., 450., 2.)
                .unwrap()
                .unwrap()
                .width,
            1600
        );
    }
}
