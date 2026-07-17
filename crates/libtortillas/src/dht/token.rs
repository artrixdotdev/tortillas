use std::{
   net::IpAddr,
   time::{Duration, Instant},
};

use rand::random;
use sha1::{Digest, Sha1};

const TOKEN_LEN: usize = 8;

/// Generates short-lived tokens that bind `announce_peer` requests to the
/// address that previously issued `get_peers`.
#[derive(Debug)]
pub struct TokenManager {
   current_secret: [u8; 20],
   previous_secret: [u8; 20],
   rotated_at: Instant,
   rotation_interval: Duration,
}

impl TokenManager {
   pub fn new(rotation_interval: Duration) -> Self {
      Self {
         current_secret: random(),
         previous_secret: random(),
         rotated_at: Instant::now(),
         rotation_interval,
      }
   }

   pub fn generate(&mut self, ip: IpAddr) -> Vec<u8> {
      self.rotate_if_needed();
      token(&self.current_secret, ip)
   }

   pub fn verify(&mut self, candidate: &[u8], ip: IpAddr) -> bool {
      self.rotate_if_needed();
      candidate == token(&self.current_secret, ip) || candidate == token(&self.previous_secret, ip)
   }

   fn rotate_if_needed(&mut self) {
      if self.rotated_at.elapsed() < self.rotation_interval {
         return;
      }
      self.previous_secret = self.current_secret;
      self.current_secret = random();
      self.rotated_at = Instant::now();
   }
}

fn token(secret: &[u8; 20], ip: IpAddr) -> Vec<u8> {
   let mut hasher = Sha1::new();
   hasher.update(secret);
   match ip {
      IpAddr::V4(ip) => hasher.update(ip.octets()),
      IpAddr::V6(ip) => hasher.update(ip.octets()),
   }
   hasher.finalize()[..TOKEN_LEN].to_vec()
}

#[cfg(test)]
mod tests {
   use super::*;

   #[test]
   fn token_when_generated_for_address_then_verifies_only_for_that_address() {
      let mut manager = TokenManager::new(Duration::from_secs(60));
      let ip = "192.0.2.10".parse().unwrap();
      let candidate = manager.generate(ip);

      assert!(manager.verify(&candidate, ip));
      assert!(!manager.verify(&candidate, "192.0.2.11".parse().unwrap()));
   }

   #[test]
   fn token_when_secret_rotates_then_accepts_previous_generation() {
      let mut manager = TokenManager::new(Duration::from_secs(60));
      let ip = "198.51.100.5".parse().unwrap();
      let candidate = manager.generate(ip);
      manager.rotated_at = Instant::now() - Duration::from_secs(61);

      let replacement = manager.generate(ip);

      assert_ne!(candidate, replacement);
      assert!(manager.verify(&candidate, ip));
   }
}
