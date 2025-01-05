use super::ShortEventId;

impl rand::distributions::Distribution<ShortEventId> for rand::distributions::Standard {
    fn sample<R: rand::Rng + ?Sized>(&self, rng: &mut R) -> ShortEventId {
        let mut bytes = [0u8; 16];
        rng.fill_bytes(&mut bytes);
        ShortEventId(bytes)
    }
}
