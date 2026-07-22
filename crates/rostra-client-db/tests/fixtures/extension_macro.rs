rostra_client_db::define_extension_table!(
    cache_entries, "example.org/cache_entries": u64 => String
);

fn main() {
    assert_eq!(cache_entries::TABLE.name(), "example.org/cache_entries");
}
