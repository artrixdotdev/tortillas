# tortillas

A terminal TUI torrent client.

## Roadmap

This serves as a very general and broad roadmap of what we intend to do with this library/CLI. If you see anything that you would like to contribute to, please see `CONTRIBUTING.md`! We're always happy to have a bit of help.

We plan to support:

- uTP peer connections
- TCP peer connections

We currently support:

- Handling Magnet URIs
- Handling Torrent files

We are currently working on (this may be a little bit out of date, feel free to ask/open an issue):

- uTP peer connections
- TCP peer connections

We do NOT plan to support:

- WebTorrent connections: Until WebTorrent's documentation and source code is greatly improved, it is not worth the time and energy required to decipher the many protocols and undocumented spec(s) that WebTorrent utilizes (SDP, DTLS, WebRTC -- more specifically how WebTorrent treats and uses these protocols).

## Testing

We use [Nextest](https://nexte.st/) for testing. You may have to install Nextest locally on your machine in order to run tests. You can see how to do so [here](https://nexte.st/docs/installation/pre-built-binaries/).

![Alt](https://repobeats.axiom.co/api/embed/2937f666319e74a9467ef1d5442edf89beabd516.svg "Repobeats analytics image")

## Usage

Please keep in mind that as of April 6th, 2025, this library is not complete.

### Handshaking with peers

#### uTP

Given a vector of peers `peers`:

```rs

         // Create a single uTP transport instance
         let mut utp_transport_handler = UtpTransportHandler::new(//...);

         // Get the tx from the created uTP transport
         let tx = utp_transport_handler.tx.clone();

         // Create a vector to hold all the join handles
         let mut join_set = JoinSet::new();

         for peer in peers {
            // Clone tx (see Tokio docs on why we need to clone tx: <https://tokio.rs/tokio/tutorial/channels>)
            let tx = tx.clone();
            join_set.spawn(async move {
               let cmd = TransportCommand::Connect { peer };

               match tx.send(cmd).await {
                  Ok(()) => Ok(peer_id),
                  Err(_) => Err("Connection failed".to_string()),
               }
            });
         }

         // tx and rx used for communication between utp_transport_handler.handle_message and this thread
         let (tx, mut rx) = mpsc::channel(100);

         // Start handling mpsc messages from the join set
         tokio::spawn(async move {
            utp_transport_handler.handle_message(tx).await.unwrap();
         });

         // Await the join_set.spawn()
         join_set.join_all().await;

         // Collect responses from handle_message
         tokio::spawn(async move {
            let mut total_peers_seen = 0;
            while let Some(_res) = rx.recv().await {
               // Do something
            }
         })
         .await
         .unwrap();
```

Please keep in mind that this code is heavily redacted. See `test_utp_peer_handshake` for more.
