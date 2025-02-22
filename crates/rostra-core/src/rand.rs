use rand::rngs::OsRng;
use rand::Rng as _;

use super::ShortEventId;

impl rand::distributions::Distribution<ShortEventId> for rand::distributions::Standard {
    fn sample<R: rand::Rng + ?Sized>(&self, rng: &mut R) -> ShortEventId {
        let mut bytes = [0u8; 16];
        rng.fill_bytes(&mut bytes);
        ShortEventId(bytes)
    }
}
impl ShortEventId {
    pub fn random() -> Self {
        let mut csprng = OsRng;
        csprng.r#gen()
    }
}
