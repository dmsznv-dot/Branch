use media_engine::EngineError;
pub trait CloseActions {
    fn cancel(&mut self);
    fn join_render(&mut self) -> impl std::future::Future<Output = Result<(), EngineError>> + Send;
    fn destroy_gl(&mut self) -> impl std::future::Future<Output = Result<(), EngineError>> + Send;
    fn release_window(&mut self);
}
#[derive(Default)]
pub struct CloseSequence {
    closed: bool,
}
impl CloseSequence {
    pub async fn close(&mut self, actions: &mut impl CloseActions) -> Result<(), EngineError> {
        if self.closed {
            return Ok(());
        }
        actions.cancel();
        actions.join_render().await?;
        actions.destroy_gl().await?;
        actions.release_window();
        self.closed = true;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Scheduler {
        steps: Vec<&'static str>,
        blocked: bool,
    }
    impl CloseActions for Scheduler {
        fn cancel(&mut self) {
            self.steps.push("cancel");
        }
        async fn join_render(&mut self) -> Result<(), EngineError> {
            self.steps.push("render joined");
            if self.blocked {
                Err(EngineError::ShutdownTimeout)
            } else {
                Ok(())
            }
        }
        async fn destroy_gl(&mut self) -> Result<(), EngineError> {
            self.steps.push("GL destroyed");
            Ok(())
        }
        fn release_window(&mut self) {
            self.steps.push("window released");
        }
    }
    #[tokio::test]
    async fn close_during_render() {
        let mut order = CloseSequence::default();
        let mut scheduler = Scheduler::default();
        order.close(&mut scheduler).await.unwrap();
        order.close(&mut scheduler).await.unwrap();
        assert_eq!(
            scheduler.steps,
            ["cancel", "render joined", "GL destroyed", "window released"]
        );
    }
    #[tokio::test]
    async fn timeout_retains_live_resources() {
        let mut order = CloseSequence::default();
        let mut scheduler = Scheduler {
            blocked: true,
            ..Default::default()
        };
        assert_eq!(
            order.close(&mut scheduler).await,
            Err(EngineError::ShutdownTimeout)
        );
        assert_eq!(scheduler.steps, ["cancel", "render joined"]);
    }
}
