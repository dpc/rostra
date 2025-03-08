use std::{cmp, ops};

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
        prefix: ops::RangeInclusive<K>,
        cursor_to_key: impl Fn(C) -> K,
        cursor: Option<C>,
        limit: usize,
        filter_fn: impl Fn(K, V) -> DbResult<Option<R>> + Send + 'static,
    ) -> DbResult<(Vec<R>, Option<K>)>
    where
        K: bincode::Decode + bincode::Encode + cmp::Ord,
        V: bincode::Decode + bincode::Encode,
    {
        let mut ret = vec![];

        let (prefix_start, prefix_end) = prefix.into_inner();

        let start = if let Some(cursor) = cursor {
            cursor_to_key(cursor).max(prefix_start)
        } else {
            prefix_start
        };

        for event in table.range(&start..=&prefix_end)? {
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
        prefix: ops::RangeInclusive<K>,
        cursor_to_key: impl Fn(C) -> K,
        cursor: Option<C>,
        limit: usize,
        filter_fn: impl Fn(K, V) -> DbResult<Option<R>> + Send + 'static,
    ) -> DbResult<(Vec<R>, Option<K>)>
    where
        K: bincode::Decode + bincode::Encode + cmp::Ord,
        V: bincode::Decode + bincode::Encode,
    {
        let mut ret = vec![];

        let (prefix_start, prefix_end) = prefix.into_inner();

        let end = if let Some(cursor) = cursor {
            cursor_to_key(cursor).min(prefix_end)
        } else {
            prefix_end
        };
        for event in table.range(&prefix_start..&end)?.rev() {
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
