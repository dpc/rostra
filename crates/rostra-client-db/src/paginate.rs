use crate::{Database, DbResult};

impl Database {
    pub fn paginate_table<K, V, R>(
        table: &impl redb_bincode::ReadableTable<K, V>,
        cursor: Option<K>,
        limit: usize,
        filter_fn: impl Fn(K, V) -> DbResult<Option<R>> + Send + 'static,
    ) -> DbResult<(Vec<R>, Option<K>)>
    where
        K: bincode::Decode + bincode::Encode,
        V: bincode::Decode + bincode::Encode,
    {
        let mut ret = vec![];

        for event in if let Some(cursor) = cursor {
            table.range(&cursor..)?
        } else {
            table.range(..)?
        } {
            let (k, v) = event?;

            let k = k.value();
            if limit <= ret.len() {
                return Ok((ret, Some(k)));
            }

            if let Some(r) = filter_fn(k, v.value())? {
                ret.push(r);
            }
        }

        Ok((ret, None))
    }

    pub fn paginate_table_rev<K, V, R>(
        table: &impl redb_bincode::ReadableTable<K, V>,
        cursor: Option<K>,
        limit: usize,
        filter_fn: impl Fn(K, V) -> DbResult<Option<R>> + Send + 'static,
    ) -> DbResult<(Vec<R>, Option<K>)>
    where
        K: bincode::Decode + bincode::Encode,
        V: bincode::Decode + bincode::Encode,
    {
        let mut ret = vec![];

        for event in if let Some(cursor) = cursor {
            table.range(..=&cursor)?
        } else {
            table.range(..)?
        }
        .rev()
        {
            let (k, v) = event?;

            let k = k.value();
            if limit <= ret.len() {
                return Ok((ret, Some(k)));
            }

            if let Some(r) = filter_fn(k, v.value())? {
                ret.push(r);
            }
        }

        Ok((ret, None))
    }

    pub fn paginate_table_partition<K, V, C, R>(
        table: &impl redb_bincode::ReadableTable<K, V>,
        prefix_cursor_min: impl Fn(Option<C>) -> K,
        prefix_max: K,
        cursor: Option<C>,
        limit: usize,
        filter_fn: impl Fn(K, V) -> DbResult<Option<R>> + Send + 'static,
    ) -> DbResult<(Vec<R>, Option<K>)>
    where
        K: bincode::Decode + bincode::Encode,
        V: bincode::Decode + bincode::Encode,
    {
        let mut ret = vec![];

        for event in table.range(&prefix_cursor_min(cursor)..=&prefix_max)? {
            let (k, v) = event?;

            let k = k.value();
            if limit <= ret.len() {
                return Ok((ret, Some(k)));
            }

            if let Some(r) = filter_fn(k, v.value())? {
                ret.push(r);
            }
        }

        Ok((ret, None))
    }

    pub fn paginate_table_partition_rev<K, V, C, R>(
        table: &impl redb_bincode::ReadableTable<K, V>,
        prefix_min: K,
        prefix_max: K,
        cursor_to_prefix: impl Fn(C) -> K,
        cursor: Option<C>,
        limit: usize,
        filter_fn: impl Fn(K, V) -> DbResult<Option<R>> + Send + 'static,
    ) -> DbResult<(Vec<R>, Option<K>)>
    where
        K: bincode::Decode + bincode::Encode,
        V: bincode::Decode + bincode::Encode,
    {
        let mut ret = vec![];

        for event in table
            .range(&prefix_min..&cursor.map(cursor_to_prefix).unwrap_or(prefix_max))?
            .rev()
        {
            let (k, v) = event?;

            let k = k.value();
            if limit <= ret.len() {
                return Ok((ret, Some(k)));
            }

            if let Some(r) = filter_fn(k, v.value())? {
                ret.push(r);
            }
        }

        Ok((ret, None))
    }
}

#[cfg(test)]
mod tests;
