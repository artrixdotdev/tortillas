use std::path::PathBuf;

use bytes::Bytes;
use kameo::{
   Actor,
   actor::ActorRef,
   prelude::{Context, Message},
};

use crate::{errors::TorrentError, hashes::Hash, torrent::util};

pub(crate) struct PieceStoreActor;

impl Actor for PieceStoreActor {
   type Args = ();
   type Error = TorrentError;

   async fn on_start(_: Self::Args, _: ActorRef<Self>) -> Result<Self, Self::Error> {
      Ok(Self)
   }
}

#[derive(Debug)]
pub(crate) enum PieceStoreMessage {
   WriteBlock {
      path: PathBuf,
      offset: usize,
      block: Bytes,
   },
}

impl Message<PieceStoreMessage> for PieceStoreActor {
   type Reply = std::io::Result<()>;

   async fn handle(
      &mut self, msg: PieceStoreMessage, _: &mut Context<Self, Self::Reply>,
   ) -> Self::Reply {
      match msg {
         PieceStoreMessage::WriteBlock {
            path,
            offset,
            block,
         } => util::write_block_to_file(path, offset, block).await,
      }
   }
}

#[derive(Debug)]
pub(crate) enum PieceStoreRequest {
   ValidateAndRead { path: PathBuf, hash: Hash<20> },
}

impl Message<PieceStoreRequest> for PieceStoreActor {
   type Reply = anyhow::Result<Bytes>;

   async fn handle(
      &mut self, msg: PieceStoreRequest, _: &mut Context<Self, Self::Reply>,
   ) -> Self::Reply {
      match msg {
         PieceStoreRequest::ValidateAndRead { path, hash } => {
            async {
               util::validate_piece_file(path.clone(), hash).await?;
               Ok(tokio::fs::read(&path).await?.into())
            }
            .await
         }
      }
   }
}
