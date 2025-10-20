use rand::Rng as _;

use super::ShortEventId;

impl rand::distr::Distribution<ShortEventId> for rand::distr::StandardUniform {
    fn sample<R: rand::Rng + ?Sized>(&self, rng: &mut R) -> ShortEventId {
        let mut bytes = [0u8; 16];
        rng.fill_bytes(&mut bytes);
        ShortEventId(bytes)
    }
}
impl ShortEventId {
    pub fn random() -> Self {
        rand::rng().random()
    }
}
