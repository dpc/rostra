use rostra_util_error::BoxedErrorResult;

use crate::tests::temp_db_rng;
use crate::{def_table, Database};

def_table!(test_table: u64 => String);

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn paginate() -> BoxedErrorResult<()> {
    let (_dir, db) = temp_db_rng().await?;

    db.write_with(|tx| {
        let mut table = tx.open_table(&test_table::TABLE)?;

        table.insert(&0, "Zero")?;
        table.insert(&3, "Three")?;
        table.insert(&7, "Seven")?;

        assert_eq!(
            Database::paginate_table(&table, None, 0, |k, v| Ok(Some(format!("{k}-{v}"))))?,
            (vec![], Some(0))
        );

        assert_eq!(
            Database::paginate_table(&table, None, 1, |k, v| Ok(Some(format!("{k}-{v}"))))?,
            (vec!["0-Zero".into()], Some(3))
        );

        assert_eq!(
            Database::paginate_table(&table, Some(0), 0, |k, v| Ok(Some(format!("{k}-{v}"))))?,
            (vec![], Some(0))
        );

        assert_eq!(
            Database::paginate_table(&table, Some(0), 1, |k, v| Ok(Some(format!("{k}-{v}"))))?,
            (vec!["0-Zero".into()], Some(3))
        );

        assert_eq!(
            Database::paginate_table(&table, Some(3), 2, |k, v| Ok(Some(format!("{k}-{v}"))))?,
            (vec!["3-Three".into(), "7-Seven".into(),], None)
        );

        assert_eq!(
            Database::paginate_table(&table, Some(3), 3, |k, v| Ok(Some(format!("{k}-{v}"))))?,
            (vec!["3-Three".into(), "7-Seven".into(),], None)
        );

        assert_eq!(
            Database::paginate_table(&table, Some(40), 3, |k, v| Ok(Some(format!("{k}-{v}"))))?,
            (vec![], None)
        );

        assert_eq!(
            Database::paginate_table(&table, None, 2, |k, v| Ok((k % 2 == 0).then_some(v)))?,
            (vec!["Zero".into()], None)
        );

        Ok(())
    })
    .await?;

    Ok(())
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn paginate_rev() -> BoxedErrorResult<()> {
    let (_dir, db) = temp_db_rng().await?;

    db.write_with(|tx| {
        let mut table = tx.open_table(&test_table::TABLE)?;

        table.insert(&0, "Zero")?;
        table.insert(&3, "Three")?;
        table.insert(&7, "Seven")?;

        assert_eq!(
            Database::paginate_table_rev(&table, None, 0, |k, v| Ok(Some(format!("{k}-{v}"))))?,
            (vec![], Some(7))
        );

        assert_eq!(
            Database::paginate_table_rev(&table, None, 1, |k, v| Ok(Some(format!("{k}-{v}"))))?,
            (vec!["7-Seven".into()], Some(3))
        );

        assert_eq!(
            Database::paginate_table_rev(&table, Some(7), 0, |k, v| Ok(Some(format!("{k}-{v}"))))?,
            (vec![], Some(7))
        );

        assert_eq!(
            Database::paginate_table_rev(&table, Some(7), 1, |k, v| Ok(Some(format!("{k}-{v}"))))?,
            (vec!["7-Seven".into()], Some(3))
        );

        assert_eq!(
            Database::paginate_table_rev(&table, Some(3), 2, |k, v| Ok(Some(format!("{k}-{v}"))))?,
            (vec!["3-Three".into(), "0-Zero".into(),], None)
        );

        assert_eq!(
            Database::paginate_table_rev(&table, Some(3), 3, |k, v| Ok(Some(format!("{k}-{v}"))))?,
            (vec!["3-Three".into(), "0-Zero".into(),], None)
        );

        assert_eq!(
            Database::paginate_table_rev(&table, None, 2, |k, v| Ok((k % 2 == 0).then_some(v)))?,
            (vec!["Zero".into()], None)
        );

        Ok(())
    })
    .await?;

    Ok(())
}
