use std::{cmp, fmt};

use super::{IrohNodeId, NodeAnnouncement};

fn round_trip<T>(v: T)
where
    T: ::serde::Serialize + for<'de> ::serde::Deserialize<'de> + fmt::Debug + cmp::Eq,
{
    let json = serde_json::to_string(&v).expect("Can't fail");
    println!("json: {}", json);
    let json_ret = serde_json::from_str(&json).expect("Can't fail");
    assert_eq!(v, json_ret);

    let mut bytes = vec![];
    cbor4ii::serde::to_writer(&mut bytes, &v).expect("ok");
    // ciborium::into_writer(&v, &mut bytes).expect("ok");
    println!("cbor: {}", data_encoding::HEXLOWER.encode(&bytes));
    let ret: T = cbor4ii::serde::from_slice(bytes.as_slice()).expect("ok");
    // let ret: T = ciborium::from_reader(&mut bytes.as_slice()).expect("ok");
    assert_eq!(v, ret);
}

#[test]
fn sanity_check_node_annocment() {
    let node_id = IrohNodeId::MAX;
    round_trip(node_id);

    let ann = NodeAnnouncement::Iroh { addr: node_id };
    round_trip(ann);
}
