use super::super::{CoreEventKind, EngineView, EventSubscription, FrontendPublisher};
use crate::engine::EngineStatus;

impl FrontendPublisher {
   pub(crate) fn subscribe(&self) -> EventSubscription {
      self.hub().engine.live.subscribe()
   }

   /// Derives the root projection from engine lifecycle and registered child
   /// scopes. The root never caches torrent views.
   pub(crate) fn view(&self) -> EngineView {
      let hub = self.hub();
      let mut torrents = hub
         .torrents
         .values()
         .into_iter()
         .filter(|scope| scope.is_registered())
         .filter_map(|scope| scope.live.view())
         .collect::<Vec<_>>();
      torrents.sort_by(|left, right| left.info_hash.as_bytes().cmp(right.info_hash.as_bytes()));
      EngineView {
         status: hub.engine.live.view(),
         torrents,
      }
   }

   pub(crate) fn engine_started(&self) {
      let hub = self.hub();
      let _ = hub.engine.live.set_view(EngineStatus::Running);
      let _ = hub
         .engine
         .live
         .publish(CoreEventKind::EngineStarted(self.view()));
   }

   pub(crate) fn engine_stopping(&self) {
      let _ = self.hub().engine.live.set_view(EngineStatus::Stopping);
   }

   pub(crate) fn engine_stopped(&self) {
      let mut view = self.view();
      view.status = EngineStatus::Stopped;
      let _ = self
         .hub()
         .engine
         .live
         .close(EngineStatus::Stopped, CoreEventKind::Shutdown(view));
   }
}
