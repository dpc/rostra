use redb_bincode::BINCODE_CONFIG;
use rostra_client_db::{
    Database, DbError, EXTENSION_RESERVED_TABLE_PREFIXES, ExtensionTableDefinition,
    define_extension_table,
};
use rostra_core::id::RostraIdSecretKey;

define_extension_table! {
    persisted_extension, "tests::extension_boundary::persisted": u64 => String
}

#[tokio::test(flavor = "multi_thread")]
async fn extension_tables_persist_without_exposing_builtin_mutation() -> anyhow::Result<()> {
    const EXPECTED_RESERVED_PREFIXES: &[&str] = &[
        "_total_migration_",
        "content_",
        "db_",
        "events_",
        "ids_",
        "reception_order_",
        "shoutbox_",
        "social_",
    ];
    assert_eq!(
        EXTENSION_RESERVED_TABLE_PREFIXES,
        EXPECTED_RESERVED_PREFIXES
    );

    let dir = tempfile::tempdir()?;
    let path = dir.path().join("db.redb");
    let self_id = RostraIdSecretKey::generate().id();
    let db = Database::open(&path, self_id).await?;

    db.extension_write(|tx| {
        tx.open_table(&persisted_extension::TABLE)?
            .insert(&7, &"persisted".to_owned())?;
        Ok(())
    })
    .await?;
    drop(db);

    let db = Database::open(&path, self_id).await?;
    let value = db
        .extension_read(|tx| {
            Ok(tx
                .open_table(&persisted_extension::TABLE)?
                .get(&7)?
                .map(|value| value.value()))
        })
        .await?;
    assert_eq!(value.as_deref(), Some("persisted"));

    for reserved_name in std::iter::once("events".to_owned()).chain(
        EXPECTED_RESERVED_PREFIXES
            .iter()
            .map(|prefix| format!("{prefix}probe")),
    ) {
        let definition = ExtensionTableDefinition::<u64, String>::new(&reserved_name);
        let error = db
            .extension_write(|tx| {
                tx.open_table(&persisted_extension::TABLE)?
                    .insert(&8, &"must roll back".to_owned())?;
                tx.open_table(&definition)?;
                Ok(())
            })
            .await
            .expect_err("built-in table names and prefixes must be inaccessible");
        assert!(matches!(
            error,
            DbError::ReservedExtensionTable { ref name } if name == &reserved_name
        ));
        assert!(
            db.extension_read(|tx| Ok(tx
                .open_table(&persisted_extension::TABLE)?
                .get(&8)?
                .is_none()))
                .await?,
            "reserved-name failure must roll back extension writes"
        );
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn extension_decode_rejects_trailing_key_and_value_bytes() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("db.redb");
    let self_id = RostraIdSecretKey::generate().id();
    let db = Database::open(&path, self_id).await?;

    db.extension_write(|tx| {
        tx.open_table(&persisted_extension::TABLE)?
            .insert(&7, &"canonical".to_owned())?;
        Ok(())
    })
    .await?;
    drop(db);

    let canonical_key = bincode::encode_to_vec(7u64, BINCODE_CONFIG)?;
    let canonical_value = bincode::encode_to_vec("canonical".to_owned(), BINCODE_CONFIG)?;
    let second_key = bincode::encode_to_vec(8u64, BINCODE_CONFIG)?;
    let mut trailing_key = canonical_key.clone();
    trailing_key.push(0);
    let mut trailing_value = canonical_value.clone();
    trailing_value.push(0);

    let raw_db = redb::Database::open(&path)?;
    let write = raw_db.begin_write()?;
    {
        let definition =
            redb::TableDefinition::<&[u8], &[u8]>::new(persisted_extension::TABLE.name());
        let mut table = write.open_table(definition)?;
        table.insert(trailing_key.as_slice(), canonical_value.as_slice())?;
        table.insert(second_key.as_slice(), trailing_value.as_slice())?;
    }
    write.commit()?;
    drop(raw_db);

    let db = Database::open(&path, self_id).await?;
    db.extension_read(|tx| {
        let table = tx.open_table(&persisted_extension::TABLE)?;

        let canonical = table.range(7..=7)?.collect::<Result<Vec<_>, _>>()?;
        assert_eq!(canonical.len(), 1);
        assert_eq!(canonical[0].0.value_try().expect("canonical key"), 7);
        assert_eq!(
            canonical[0].1.value_try().expect("canonical value"),
            "canonical"
        );

        assert!(
            table
                .get(&8)?
                .expect("raw trailing-value row")
                .value_try()
                .is_err()
        );
        let malformed_iteration = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            table
                .range::<u64>(..)
                .expect("range opens")
                .map(|entry| entry.expect("storage remains readable").1.value())
                .collect::<Vec<_>>()
        }));
        assert!(malformed_iteration.is_err());
        Ok(())
    })
    .await?;

    Ok(())
}
