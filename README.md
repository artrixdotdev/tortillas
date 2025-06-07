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
            let tx = utp_transport_handler.tx.clone();

            // This is one way that UtpTransports could be handled async
            // This tidbit is just for confirming that we successfully connected to at least one
            // peer
            let (success_tx, mut success_rx) = mpsc::channel(100);

            // For each peer, spawn a task to connect
            for peer in peers {
               // Clone tx (see Tokio docs on why we need to clone tx: <https://tokio.rs/tokio/tutorial/channels>)
               let tx = tx.clone();
               let success_tx_clone = success_tx.clone();
               tokio::spawn(async move {
                  let (oneshot_tx, oneshot_rx) = oneshot::channel();
                  let cmd = TransportCommand::Connect { peer, oneshot_tx };

                  tx.send(cmd).await.unwrap();

                  // Receive message. There is no error handling present here as there's no reason
                  // to -- all we're doing is handshaking, and then ending the process. If you'd
                  // like to see a more rigorous (perhaps) way of handling errors, take a look at
                  // the torrent() function in TorrentEngine
                  success_tx_clone
                     .send(oneshot_rx.await.unwrap())
                     .await
                     .unwrap();
               });
            }

            // Start handling mpsc messages from the join set
            tokio::spawn(async move {
               utp_transport_handler.handle_commands().await.unwrap();
            });

            // As long as this unwraps correctly, we have successfully made a handshake.
            let res = success_rx.recv().await.unwrap().unwrap();
            trace!("{:?}", res);
```

Please keep in mind that this code is heavily redacted and is not an entirely optimal way to do this (specifically in reference to error handling). See `test_utp_peer_handshake` for more.
