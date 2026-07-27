use std::{error::Error, time::Instant};

use bytes::Bytes;
use libtortillas::protocol::{
   messages::PeerMessages,
   stream::{PeerRecv, PeerSend, PeerStream},
};
use tokio::net::{TcpListener, TcpStream};
use tracing::info;

const BLOCK_LENGTH: usize = 16 * 1024;
const BLOCK_COUNT: usize = 8 * 1024;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
   tracing_subscriber::fmt().init();

   let listener = TcpListener::bind("127.0.0.1:0").await?;
   let address = listener.local_addr()?;
   let receiver = tokio::spawn(async move {
      let (stream, _) = listener.accept().await?;
      let mut stream = PeerStream::tcp(stream);
      let mut bytes_received = 0usize;

      for expected_index in 0..BLOCK_COUNT {
         match stream.recv().await? {
            PeerMessages::Piece(index, 0, block)
               if index == expected_index as u32 && block.len() == BLOCK_LENGTH =>
            {
               bytes_received += block.len();
            }
            message => {
               return Err(format!("received unexpected message: {message}").into());
            }
         }
      }

      Ok::<_, Box<dyn Error + Send + Sync>>(bytes_received)
   });

   let block = Bytes::from(vec![0xa5; BLOCK_LENGTH]);
   let mut sender = PeerStream::tcp(TcpStream::connect(address).await?);
   let started_at = Instant::now();

   for index in 0..BLOCK_COUNT {
      sender
         .send(PeerMessages::Piece(index as u32, 0, block.clone()))
         .await?;
   }

   let bytes_transferred = receiver.await??;
   let elapsed = started_at.elapsed();
   let mebibytes = bytes_transferred as f64 / (1024.0 * 1024.0);
   let throughput = mebibytes / elapsed.as_secs_f64();
   info!(
      bytes_transferred,
      ?elapsed,
      throughput_mib_per_second = throughput,
      "completed local peer transfer"
   );

   Ok(())
}
