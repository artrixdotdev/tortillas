use std::{io::Result as IoResult, path::PathBuf};

use bytes::Bytes;
use kameo::{Actor, actor::ActorRef, messages};
use tokio::fs::read;

use crate::{errors::TorrentError, hashes::Hash, torrent::util};

pub(crate) struct PieceStoreActor;

impl Actor for PieceStoreActor {
   type Args = ();
   type Error = TorrentError;

   async fn on_start(_: Self::Args, _: ActorRef<Self>) -> Result<Self, Self::Error> {
      Ok(Self)
   }
}

#[messages]
impl PieceStoreActor {
   #[message(derive(Debug))]
   pub(crate) async fn write_block(
      &mut self, path: PathBuf, offset: usize, block: Bytes,
   ) -> IoResult<()> {
      util::write_block_to_file(path, offset, block).await
   }

   #[message(derive(Debug))]
   pub(crate) async fn validate_and_read(
      &mut self, path: PathBuf, hash: Hash<20>,
   ) -> anyhow::Result<Bytes> {
      util::validate_piece_file(path.clone(), hash).await?;
      Ok(read(&path).await?.into())
   }
}
