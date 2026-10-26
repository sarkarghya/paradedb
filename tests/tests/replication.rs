// Copyright (c) 2023-2026 ParadeDB, Inc.
//
// This file is part of ParadeDB - Postgres for Search and Analytics
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program. If not, see <http://www.gnu.org/licenses/>.

use anyhow::Result;
use cmd_lib::{run_cmd, run_fun};
use dotenvy::dotenv;
use rstest::*;
use sqlx::{Connection, Executor, PgConnection};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tests::fixtures::db::Query;

// Static variables for initializing port assignment and ensuring one-time setup
static INIT: Once = Once::new();
static LAST_PORT: AtomicUsize = AtomicUsize::new(49152);

const RETRIES: u32 = 60;
const RETRY_DELAY: u64 = 1000; // measured in milliseconds

// Function to check if a port can be bound (i.e., is available)
fn can_bind(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

// Function to get a free port in the dynamic port range
fn get_free_port() -> u16 {
    let port_upper_bound = 65535;
    let port_lower_bound = 49152;

    INIT.call_once(|| {
        LAST_PORT.store(port_lower_bound, Ordering::SeqCst);
    });

    loop {
        let port = LAST_PORT.fetch_add(1, Ordering::SeqCst);
        if port > port_upper_bound {
            LAST_PORT.store(port_lower_bound, Ordering::SeqCst);
            continue;
        }

        if can_bind(port as u16) {
            return port as u16;
        }
    }
}

// Superuser created by `initdb` for every ephemeral instance.
//
// This is pinned rather than left to `initdb`'s default (the OS user) because sqlx
// resolves an omitted username via `whoami`, which reports `anonymous` when neither
// `USER` nor `LOGNAME` is set — as is the case in our CI containers. Naming the role on
// both sides keeps the tests independent of the ambient environment.
const SUPERUSER: &str = "postgres";

// Struct to manage an ephemeral PostgreSQL instance
struct EphemeralPostgres {
    pub tempdir_path: String,
    pub host: String,
    pub port: u16,
    pub dbname: String,
    pub pg_ctl_path: PathBuf,
    pub log_path: PathBuf,
}

// Implement Drop trait to ensure the PostgreSQL instance is properly stopped
impl Drop for EphemeralPostgres {
    fn drop(&mut self) {
        let path = &self.tempdir_path;
        let pg_ctl_path = &self.pg_ctl_path;
        run_cmd!($pg_ctl_path -D $path stop &> /dev/null)
            .unwrap_or_else(|_| println!("postgres instance at {} already shut down", self.port));
        std::fs::remove_dir_all(self.tempdir_path.clone()).unwrap();
    }
}

// Implementation of EphemeralPostgres
impl EphemeralPostgres {
    fn pg_bin_path() -> PathBuf {
        let pg_config_path = std::env::var("PG_CONFIG").expect(
            "PG_CONFIG variable must be set to enable creating ephemeral Postgres instances",
        );
        if !PathBuf::from(&pg_config_path).exists() {
            panic!(
                "PG_CONFIG variable must be a valid path to enable creating ephemeral Postgres instances, received {pg_config_path}"
            );
        }
        match run_fun!($pg_config_path --bindir) {
            Ok(path) => PathBuf::from(path.trim().to_string()),
            Err(err) => panic!("could run pg_config --bindir to get Postgres bin folder: {err}"),
        }
    }

    fn pg_basebackup_path() -> PathBuf {
        Self::pg_bin_path().join("pg_basebackup")
    }

    fn initdb_path() -> PathBuf {
        Self::pg_bin_path().join("initdb")
    }

    fn pg_ctl_path() -> PathBuf {
        Self::pg_bin_path().join("pg_ctl")
    }

    fn new_from_initialized(
        tempdir_path: &Path,
        postgresql_conf: Option<&str>,
        pg_hba_conf: Option<&str>,
    ) -> Self {
        let tempdir_path = tempdir_path.to_str().unwrap().to_string();
        let port = get_free_port();
        let pg_ctl_path = Self::pg_ctl_path();

        // Write to postgresql.conf
        let config_content = match postgresql_conf {
            Some(config) => format!("port = {}\n{}", port, config.trim()),
            None => format!("port = {port}"),
        };
        let config_path = format!("{tempdir_path}/postgresql.conf");
        std::fs::write(config_path, config_content).expect("Failed to write to postgresql.conf");

        // Write to pg_hba.conf
        if let Some(config_content) = pg_hba_conf {
            let config_path = format!("{tempdir_path}/pg_hba.conf");

            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(config_path)
                .expect("Failed to open pg_hba.conf");

            writeln!(file, "{config_content}").expect("Failed to append to pg_hba.conf");
        }

        // Create log directory
        let timestamp = chrono::Utc::now().timestamp_millis();
        let logfile = format!("/tmp/ephemeral_postgres_logs/{timestamp}.log");
        std::fs::create_dir_all(Path::new(&logfile).parent().unwrap())
            .expect("Failed to create log directory");

        // Start PostgreSQL
        run_cmd!($pg_ctl_path -D $tempdir_path -l $logfile start)
            .expect("Failed to start Postgres");

        Self {
            // TempDir needs to be stored on the struct to avoid being dropped, otherwise the
            // temp folder will be deleted before the test finishes.
            tempdir_path,
            host: "localhost".to_string(),
            port,
            dbname: "postgres".to_string(),
            pg_ctl_path,
            log_path: PathBuf::from(logfile),
        }
    }

    fn new(postgresql_conf: Option<&str>, pg_hba_conf: Option<&str>) -> Self {
        // Make sure .env files are loaded before reading env vars.
        dotenv().ok();

        let init_db_path = Self::initdb_path();
        let tempdir = TempDir::new().expect("Failed to create temp dir");
        let tempdir_path = tempdir.keep();

        // Initialize PostgreSQL data directory
        run_cmd!($init_db_path -D $tempdir_path --username $SUPERUSER &> /dev/null)
            .expect("Failed to initialize Postgres data directory");

        Self::new_from_initialized(tempdir_path.as_path(), postgresql_conf, pg_hba_conf)
    }

    // Method to establish a connection to the PostgreSQL instance
    async fn connection(&self) -> Result<PgConnection> {
        Ok(PgConnection::connect(&format!(
            "postgresql://{}@{}:{}/{}",
            SUPERUSER, self.host, self.port, self.dbname
        ))
        .await?)
    }
}

// Test function to test the ephemeral PostgreSQL setup
#[rstest]
#[async_std::test]
async fn test_logical_replication() -> Result<()> {
    let config = "
        wal_level = logical
        max_replication_slots = 4
        max_wal_senders = 4
        shared_preload_libraries = 'pg_search'
    ";

    let source_postgres = EphemeralPostgres::new(Some(config), None);
    let target_postgres = EphemeralPostgres::new(Some(config), None);

    let mut source_conn = source_postgres.connection().await?;
    let mut target_conn = target_postgres.connection().await?;

    let major_version: (i32,) = "
        SELECT split_part(setting, '.', 1)::int AS major_version
        FROM pg_catalog.pg_settings
        WHERE name = 'server_version';
        "
    .fetch_one(&mut source_conn);

    // Logical replication for versions < 17 is not implemented
    if major_version.0 < 17 {
        return Ok(());
    }

    // Create pg_search extension on both source and target databases
    "CREATE EXTENSION IF NOT EXISTS vector".execute(&mut source_conn);
    "CREATE EXTENSION IF NOT EXISTS vector".execute(&mut target_conn);
    "CREATE EXTENSION IF NOT EXISTS pg_search".execute(&mut source_conn);
    "CREATE EXTENSION IF NOT EXISTS pg_search".execute(&mut target_conn);

    // Create the mock_items table schema
    let schema = "
        CREATE TABLE mock_items (
          id SERIAL PRIMARY KEY,
          description TEXT,
          rating INTEGER CHECK (rating BETWEEN 1 AND 5),
          category VARCHAR(255),
          in_stock BOOLEAN,
          metadata JSONB,
          created_at TIMESTAMP,
          last_updated_date DATE,
          latest_available_time TIME
        )
    ";
    schema.execute(&mut source_conn);
    schema.execute(&mut target_conn);

    // Create the bm25 index on the description field
    "CREATE INDEX mock_items_bm25_idx ON public.mock_items
    USING paradedb (id, (description::pdb.unicode_words('lowercase=true')));
    "
    .execute(&mut source_conn);
    "CREATE INDEX mock_items_bm25_idx ON public.mock_items
    USING paradedb (id, (description::pdb.unicode_words('lowercase=true')));
    "
    .execute(&mut target_conn);

    // Create publication and subscription for replication
    "CREATE PUBLICATION mock_items_pub FOR TABLE mock_items".execute(&mut source_conn);
    format!(
        "CREATE SUBSCRIPTION mock_items_sub
         CONNECTION 'host={} port={} dbname={} user={}'
         PUBLICATION mock_items_pub;",
        source_postgres.host, source_postgres.port, source_postgres.dbname, SUPERUSER
    )
    .execute(&mut target_conn);

    // Verify initial state of the search results
    let source_results: Vec<(String,)> =
        "SELECT description FROM mock_items WHERE description ||| 'shoes'".fetch(&mut source_conn);
    let target_results: Vec<(String,)> =
        "SELECT description FROM mock_items WHERE description ||| 'shoes'".fetch(&mut target_conn);

    assert_eq!(source_results.len(), 0);
    assert_eq!(target_results.len(), 0);

    // Insert a new item into the source database
    "INSERT INTO mock_items (description, category, in_stock, latest_available_time, last_updated_date, metadata, created_at, rating)
    VALUES ('Red sports shoes', 'Footwear', true, '12:00:00', '2024-07-10', '{}', '2024-07-10 12:00:00', 1)".execute(&mut source_conn);

    // Verify the insert is replicated to the target database
    let source_results: Vec<(String,)> =
        "SELECT description FROM mock_items WHERE description ||| 'shoes'".fetch(&mut source_conn);

    // Wait for the replication to complete
    let target_results: Vec<(String,)> =
        "SELECT description FROM mock_items WHERE description ||| 'shoes'".fetch_retry(
            &mut target_conn,
            RETRIES,
            RETRY_DELAY,
            |result| !result.is_empty(),
        );

    assert_eq!(source_results.len(), 1);
    assert_eq!(target_results.len(), 1);

    // Additional insert test
    "INSERT INTO mock_items (description, category, in_stock, latest_available_time, last_updated_date, metadata, created_at, rating)
    VALUES ('Blue running shoes', 'Footwear', true, '14:00:00', '2024-07-10', '{}', '2024-07-10 14:00:00', 2)".execute(&mut source_conn);

    // Verify the additional insert is replicated to the target database
    let source_results: Vec<(String,)> =
        "SELECT description FROM mock_items WHERE id @@@ pdb.parse('description:\"running shoes\"')"
            .fetch(&mut source_conn);

    // Wait for the replication to complete
    std::thread::sleep(std::time::Duration::from_secs(1));
    let target_results: Vec<(String,)> =
        "SELECT description FROM mock_items WHERE id @@@ pdb.parse('description:\"running shoes\"')"
            .fetch(&mut target_conn);

    assert_eq!(source_results.len(), 1);
    assert_eq!(target_results.len(), 1);

    // Update test
    "UPDATE mock_items SET rating = 5 WHERE description = 'Red sports shoes'"
        .execute(&mut source_conn);

    // Verify the update is replicated to the target database
    let source_results: Vec<(i32,)> =
        "SELECT rating FROM mock_items WHERE description = 'Red sports shoes'"
            .fetch(&mut source_conn);

    std::thread::sleep(std::time::Duration::from_secs(5)); // give a little time for the data to replicate

    let target_results: Vec<(i32,)> =
        "SELECT rating FROM mock_items WHERE description = 'Red sports shoes'".fetch_retry(
            &mut target_conn,
            RETRIES,
            RETRY_DELAY,
            |result| !result.is_empty(),
        );

    assert_eq!(source_results.len(), 1);
    assert_eq!(target_results.len(), 1);
    assert_eq!(source_results[0], target_results[0]);

    // Delete test
    "DELETE FROM mock_items WHERE description = 'Red sports shoes'".execute(&mut source_conn);

    // Verify the delete is replicated to the target database
    let source_results: Vec<(String,)> =
        "SELECT description FROM mock_items WHERE description = 'Red sports shoes'"
            .fetch(&mut source_conn);

    let target_results: Vec<(String,)> =
        "SELECT description FROM mock_items WHERE description = 'Red sports shoes'".fetch_retry(
            &mut target_conn,
            RETRIES,
            RETRY_DELAY,
            |result| result.is_empty(),
        );

    assert_eq!(source_results.len(), 0);
    assert_eq!(target_results.len(), 0);

    // COPY test
    let mut copyin = source_conn
        .copy_in_raw("COPY mock_items(description) FROM STDIN")
        .await?;
    copyin
        .send("replicated1\nreplicated2\nreplicated3".as_bytes())
        .await?;
    copyin.finish().await?;

    // verify the COPY worked
    let source_results: Vec<(String,)> =
        "SELECT description FROM mock_items WHERE description ||| 'replicated1' OR description ||| 'replicated2' OR description ||| 'replicated3'"
            .fetch(&mut source_conn);
    assert_eq!(source_results.len(), 3);

    std::thread::sleep(std::time::Duration::from_secs(1)); // give a little time for the data to replicate
    let target_results: Vec<(String,)> =
        "SELECT description FROM mock_items WHERE description ||| 'replicated1' OR description ||| 'replicated2' OR description ||| 'replicated3'"
            .fetch_retry(&mut target_conn, RETRIES, RETRY_DELAY, |result| result.len() == 3);
    assert_eq!(target_results.len(), 3);

    Ok(())
}

#[rstest]
#[async_std::test]
async fn test_ephemeral_postgres_with_pg_basebackup() -> Result<()> {
    let config = "
        wal_level = logical
        max_replication_slots = 4
        max_wal_senders = 4
        # Adding pg_search to shared_preload_libraries in 17 doesn't do anything
        # but simplifies testing
        shared_preload_libraries = 'pg_search'
    ";

    let source_postgres = EphemeralPostgres::new(Some(config), None);
    let mut source_conn = source_postgres.connection().await?;
    let source_port = source_postgres.port;
    let source_username = "SELECT CURRENT_USER"
        .fetch_one::<(String,)>(&mut source_conn)
        .0;

    "CREATE TABLE text_array_table (
            id SERIAL PRIMARY KEY,
            text_array TEXT[]
        )"
    .execute(&mut source_conn);

    "INSERT INTO text_array_table (text_array) VALUES
        (ARRAY['apple', 'banana', 'cherry']),
        (ARRAY['dog', 'elephant', 'fox']),
        (ARRAY['grape', 'honeydew', 'kiwi']),
        (ARRAY['lion', 'monkey', 'newt']),
        (ARRAY['octopus', 'penguin', 'quail']),
        (ARRAY['rabbit', 'snake', 'tiger']),
        (ARRAY['umbrella', 'vulture', 'wolf']),
        (ARRAY['x-ray', 'yak', 'zebra']),
        (ARRAY['alpha', 'bravo', 'charlie']),
        (ARRAY['delta', 'echo', 'foxtrot'])"
        .execute(&mut source_conn);

    // Create pg_search extension and bm25 index
    "CREATE EXTENSION IF NOT EXISTS vector".execute(&mut source_conn);
    "CREATE EXTENSION IF NOT EXISTS pg_search".execute(&mut source_conn);

    "
    CREATE INDEX text_array_table_idx ON text_array_table
    USING paradedb (id, text_array);
    "
    .execute(&mut source_conn);

    // Verify search results before pg_basebackup
    let source_results: Vec<(i32,)> =
        sqlx::query_as("SELECT id FROM text_array_table WHERE text_array ||| 'dog' ORDER BY id")
            .fetch_all(&mut source_conn)
            .await?;
    assert_eq!(source_results.len(), 1);

    let target_tempdir = TempDir::new().expect("Failed to create temp dir");
    let target_tempdir_path = target_tempdir.keep();

    // Permissions for the --pgdata directory passed to pg_basebackup
    // should be u=rwx (0700) or u=rwx,g=rx (0750)
    std::fs::set_permissions(
        target_tempdir_path.as_path(),
        std::fs::Permissions::from_mode(0o700),
    )
    .expect("couldn't set permissions on target_tempdir path");

    // Run pg_basebackup
    let pg_basebackup = EphemeralPostgres::pg_basebackup_path();
    run_cmd!($pg_basebackup --pgdata $target_tempdir_path --host localhost --port $source_port --username $source_username)
    .expect("Failed to run pg_basebackup");

    let target_postgres =
        EphemeralPostgres::new_from_initialized(target_tempdir_path.as_path(), Some(config), None);
    let mut target_conn = target_postgres.connection().await?;

    // Verify the content in the target database
    let target_results: Vec<(i32,)> =
        sqlx::query_as("SELECT id FROM text_array_table WHERE text_array ||| 'dog'")
            .fetch_all(&mut target_conn)
            .await?;

    assert_eq!(source_results.len(), target_results.len());

    // Verify the table content
    let source_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM text_array_table")
        .fetch_one(&mut source_conn)
        .await?;
    let target_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM text_array_table")
        .fetch_one(&mut target_conn)
        .await?;

    assert_eq!(source_count, target_count);

    Ok(())
}

#[rstest]
#[async_std::test]
async fn test_physical_streaming_replication() -> Result<()> {
    // Create a unique directory for WAL archiving
    let archive_dir = TempDir::new().expect("Failed to create archive dir for WALs");
    // No need to set custom permissions, but you could if desired.

    // Adjust the archive_command to avoid file existence checks since this is a new directory.
    let primary_config = format!(
        "
        listen_addresses = 'localhost'
        wal_level = replica
        archive_mode = on
        archive_command = 'cp %p {}/%f'
        max_wal_senders = 3
        wal_keep_size = '160MB'
        # Adding pg_search to shared_preload_libraries in 17 doesn't do anything
        # but simplifies testing
        shared_preload_libraries = 'pg_search'
        ",
        archive_dir.path().display()
    );

    let primary_pg_hba = "
        host replication replicator 127.0.0.1/32 md5
        host replication replicator ::1/128 md5
    ";

    // Step 1: Create and start the primary Postgres instance
    let primary_postgres = EphemeralPostgres::new(Some(&primary_config), Some(primary_pg_hba));
    let mut primary_conn = primary_postgres.connection().await?;

    // Create a replication user and test table on primary
    "CREATE USER replicator WITH REPLICATION ENCRYPTED PASSWORD 'replicator_pass';"
        .execute(&mut primary_conn);
    "CREATE EXTENSION IF NOT EXISTS vector;".execute(&mut primary_conn);
    "CREATE EXTENSION IF NOT EXISTS pg_search;".execute(&mut primary_conn);
    "CREATE TABLE test_data (id SERIAL PRIMARY KEY, info TEXT);".execute(&mut primary_conn);

    // Insert initial data on primary
    "INSERT INTO test_data (info) VALUES ('initial');".execute(&mut primary_conn);

    let primary_port = primary_postgres.port;

    // Step 2: Create the standby using pg_basebackup
    let standby_tempdir = TempDir::new().expect("Failed to create temp dir for standby");
    std::fs::set_permissions(
        standby_tempdir.path(),
        std::fs::Permissions::from_mode(0o700),
    )?;

    let pg_basebackup = EphemeralPostgres::pg_basebackup_path();
    let standby_tempdir = standby_tempdir.path();
    run_cmd!(
        $pg_basebackup
        -D $standby_tempdir
        -Fp -Xs -P -R
        -h localhost
        -U replicator
        --port $primary_port
        &> /dev/null
    )
    .expect("Failed to run pg_basebackup for standby setup");

    let standby_config = "
        # Adding pg_search to shared_preload_libraries in 17 doesn't do anything
        # but simplifies testing
        shared_preload_libraries = 'pg_search'
        hot_standby = on
    ";

    // Start the standby
    let standby_postgres =
        EphemeralPostgres::new_from_initialized(standby_tempdir, Some(standby_config), None);
    let mut standby_conn = standby_postgres.connection().await?;

    // Wait a moment for the standby to catch up
    std::thread::sleep(std::time::Duration::from_secs(2));

    // Verify that the initial data replicated
    let standby_data: Vec<(String,)> = "SELECT info FROM test_data".fetch_retry(
        &mut standby_conn,
        RETRIES,
        RETRY_DELAY,
        |result| !result.is_empty(),
    );

    assert_eq!(standby_data.len(), 1);
    assert_eq!(standby_data[0].0, "initial");

    // (Optional) Insert more data on primary and verify it appears on standby
    "INSERT INTO test_data (info) VALUES ('from_primary');".execute(&mut primary_conn);

    let standby_data: Vec<(String,)> = "SELECT info FROM test_data WHERE info='from_primary'"
        .fetch_retry(&mut standby_conn, RETRIES, RETRY_DELAY, |result| {
            !result.is_empty()
        });

    assert_eq!(standby_data.len(), 1);

    // Insert a different value into the primary and ensure it streams over
    "INSERT INTO test_data (info) VALUES ('from_primary_2');".execute(&mut primary_conn);

    // Now, check for 'from_primary_2'
    let standby_data: Vec<(String,)> = "SELECT info FROM test_data WHERE info='from_primary_2'"
        .fetch_retry(&mut standby_conn, RETRIES, RETRY_DELAY, |result| {
            !result.is_empty()
        });

    assert_eq!(standby_data.len(), 1);

    // Optional: Test synchronous replication
    // Reconfigure primary to require synchronous replication
    // This ensures commits wait for replication confirmation.
    "ALTER SYSTEM SET synchronous_standby_names = '*';".execute(&mut primary_conn);
    let pg_ctl_path = primary_postgres.pg_ctl_path.clone();
    let tempdir_path = primary_postgres.tempdir_path.clone();
    run_cmd!($pg_ctl_path -D $tempdir_path restart &> /dev/null)
        .expect("Failed to restart primary with sync config");

    // Reconnect after restart
    let mut primary_conn = primary_postgres.connection().await?;
    // Insert a row, then check standby to ensure synchronous commit.
    // If no connected standby matches, the commit on the primary will block indefinitely.
    "BEGIN; INSERT INTO test_data (info) VALUES ('sync_test'); COMMIT;".execute(&mut primary_conn);

    let sync_row: Vec<(String,)> = "SELECT info FROM test_data WHERE info='sync_test'".fetch_retry(
        &mut standby_conn,
        RETRIES,
        RETRY_DELAY,
        |result| !result.is_empty(),
    );
    assert_eq!(sync_row.len(), 1);

    // Optional: Failover test - Stop primary and promote standby
    let pg_ctl_path = primary_postgres.pg_ctl_path.clone();
    let tempdir_path = primary_postgres.tempdir_path.clone();
    run_cmd!($pg_ctl_path -D $tempdir_path stop &> /dev/null).unwrap();

    // Promote standby using pg_ctl promote
    let tempdir_path = standby_postgres.tempdir_path.clone();
    let pg_ctl_path = standby_postgres.pg_ctl_path.clone();
    run_cmd!($pg_ctl_path -D $tempdir_path promote &> /dev/null)
        .expect("Failed to promote standby");

    thread::sleep(Duration::from_secs(2));
    let mut standby_conn = standby_postgres.connection().await?;
    "INSERT INTO test_data (info) VALUES ('promoted_standby');".execute(&mut standby_conn);

    // Ensure we can read back the inserted row from the now promoted standby
    let promoted_data: Vec<(String,)> = "SELECT info FROM test_data WHERE info='promoted_standby'"
        .fetch_retry(&mut standby_conn, RETRIES, RETRY_DELAY, |result| {
            !result.is_empty()
        });
    assert_eq!(promoted_data.len(), 1);

    Ok(())
}

#[rstest]
#[async_std::test]
async fn test_wal_streaming_replication_with_pg_search() -> Result<()> {
    // Primary Postgres setup + insert data
    let postgresql_conf = "
        listen_addresses = 'localhost'
        wal_level = replica
        max_wal_senders = 4
        shared_preload_libraries = 'pg_search'
        # It's often helpful to have a short wal_keep_size or max_wal_senders for testing
    ";
    let pg_hba_conf = "
        host replication all 127.0.0.1/32 md5
        host replication all ::1/128 md5
    ";
    let source_postgres = EphemeralPostgres::new(Some(postgresql_conf), Some(pg_hba_conf));
    let mut source_conn = source_postgres.connection().await?;
    let source_port = source_postgres.port;
    let source_username = "replicator";

    // Create a replication user and slot on primary
    format!("CREATE USER {source_username} WITH REPLICATION ENCRYPTED PASSWORD 'replicator_pass'")
        .execute(&mut source_conn);
    "SELECT pg_create_physical_replication_slot('wal_receiver_1');".execute(&mut source_conn);

    // Install pg_search on primary and create a test table
    "CREATE EXTENSION IF NOT EXISTS vector".execute(&mut source_conn);
    "CREATE EXTENSION IF NOT EXISTS pg_search".execute(&mut source_conn);
    "CREATE TABLE items (
        id SERIAL PRIMARY KEY,
        description TEXT,
        category TEXT,
        created_at TIMESTAMP
    )"
    .execute(&mut source_conn);

    // Insert initial data
    "INSERT INTO items (description, category, created_at) VALUES
        ('Red running shoes', 'Footwear', NOW()),
        ('Blue sports shoes', 'Footwear', NOW()),
        ('Wireless headphones', 'Electronics', NOW()),
        ('4K television', 'Electronics', NOW())"
        .execute(&mut source_conn);

    // Set up the standby using pg_basebackup
    let target_tempdir = TempDir::new().expect("Failed to create temp dir for standby");
    let target_tempdir_path = target_tempdir.keep();

    // Permissions for the --pgdata directory passed to pg_basebackup
    // should be u=rwx (0700) or u=rwx,g=rx (0750)
    std::fs::set_permissions(
        target_tempdir_path.as_path(),
        std::fs::Permissions::from_mode(0o700),
    )?;

    let pg_basebackup = EphemeralPostgres::pg_basebackup_path();
    run_cmd!(
        $pg_basebackup
        --pgdata $target_tempdir_path
        --host localhost
        --port $source_port
        --username $source_username
        -Fp -Xs -P -R
        &> /dev/null
    )
    .expect("Failed to run pg_basebackup for standby setup");

    // Start the standby also with pg_search preloaded
    let standby_config = "
        shared_preload_libraries = 'pg_search'
        hot_standby = on
        primary_slot_name = wal_receiver_1
    ";

    let standby_postgres = EphemeralPostgres::new_from_initialized(
        target_tempdir_path.as_path(),
        Some(standby_config),
        None,
    );
    let mut standby_conn = standby_postgres.connection().await?;

    // Confirm baseline streaming: the standby sees the four pre-bm25 rows.
    let standby_rows: Vec<(String,)> = "SELECT description FROM items ORDER BY id".fetch_retry(
        &mut standby_conn,
        RETRIES,
        RETRY_DELAY,
        |result| result.len() == 4,
    );
    assert_eq!(standby_rows.len(), 4);

    // ambuild emits the rmgr 137 marker. Community rejects it; the fork's
    // experimental physical-failover build must replay it without stopping recovery.
    "
    CREATE INDEX items_search_idx ON items
    USING paradedb (id, description, category);
    "
    .execute(&mut source_conn);

    // This opt-in must only be used against a library built with
    // `experimental-physical-failover`; the test below proves it really replays WAL,
    // rejects standby search reads, and uses the copied index after promotion.
    if std::env::var("PG_SEARCH_TEST_PHYSICAL_FAILOVER").as_deref() == Ok("1") {
        return test_pg_search_physical_failover(
            &source_postgres,
            &standby_postgres,
            &mut source_conn,
            &mut standby_conn,
        )
        .await;
    }

    // Wait for the marker to stream to the standby and FATAL its recovery process. Poll the
    // log instead of sleeping for a fixed duration so the test self-paces.
    let expected = "replicas are not supported on community and require paradedb enterprise";
    let log_path = standby_postgres.log_path.clone();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut tripwire_found = false;
    while std::time::Instant::now() < deadline {
        if let Ok(log) = std::fs::read_to_string(&log_path)
            && log.contains(expected)
        {
            tripwire_found = true;
            break;
        }
        thread::sleep(Duration::from_millis(200));
    }
    assert!(
        tripwire_found,
        "expected standby log {log_path:?} to contain tripwire message {expected:?}"
    );

    // The FATAL during recovery should also have brought the standby down: reconnects fail.
    let reconnect = PgConnection::connect(&format!(
        "postgresql://{}@{}:{}/{}",
        SUPERUSER, standby_postgres.host, standby_postgres.port, standby_postgres.dbname
    ))
    .await;
    assert!(
        reconnect.is_err(),
        "expected reconnect to dead standby to fail, got {reconnect:?}"
    );

    Ok(())
}

// Wait for WAL, not row counts or a fixed sleep: index, merge, and VACUUM records
// must have replayed too. Each poll is a fresh autocommit snapshot.
async fn wait_for_replay(primary: &mut PgConnection, standby: &mut PgConnection) -> Result<()> {
    let lsn: String = sqlx::query_scalar("SELECT pg_current_wal_flush_lsn()::text")
        .fetch_one(primary)
        .await?;
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let caught_up: bool = sqlx::query_scalar(
            "SELECT COALESCE(pg_last_wal_replay_lsn() >= $1::text::pg_lsn, false)",
        )
        .bind(&lsn)
        .fetch_one(&mut *standby)
        .await?;
        if caught_up {
            return Ok(());
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "standby did not replay through {lsn}"
        );
        async_std::task::sleep(Duration::from_millis(50)).await;
    }
}

async fn test_pg_search_physical_failover(
    primary: &EphemeralPostgres,
    standby: &EphemeralPostgres,
    primary_conn: &mut PgConnection,
    standby_conn: &mut PgConnection,
) -> Result<()> {
    // Ordinary relations must continue to stream after INIT_INDEX, including an
    // ordinary index on a different table. An empty search index also emits a marker.
    primary_conn
        .execute("CREATE TABLE ordinary (id int PRIMARY KEY, value text)")
        .await?;
    primary_conn
        .execute("CREATE INDEX ordinary_value_idx ON ordinary (value)")
        .await?;
    primary_conn
        .execute("INSERT INTO ordinary VALUES (1, 'after index creation')")
        .await?;
    primary_conn
        .execute("CREATE TABLE empty_items (id bigint, body text)")
        .await?;
    primary_conn
        .execute("CREATE INDEX empty_items_idx ON empty_items USING paradedb (id, body)")
        .await?;
    wait_for_replay(primary_conn, standby_conn).await?;
    let ordinary: String = sqlx::query_scalar("SELECT value FROM ordinary WHERE id = 1")
        .fetch_one(&mut *standby_conn)
        .await?;
    assert_eq!(ordinary, "after index creation");

    // No hot-standby search reads: a query error must not kill recovery, poison the
    // connection, or allow a planner/admin path to initialize legacy metadata.
    let error = standby_conn
        .execute("SELECT id FROM items WHERE description ||| 'shoes'")
        .await
        .expect_err("standby search must fail cleanly");
    let error = error
        .as_database_error()
        .expect("expected a PostgreSQL error");
    assert_eq!(error.code().as_deref(), Some("0A000"));
    assert!(error.message().contains("cannot be read during recovery"));
    let recovering: bool = sqlx::query_scalar("SELECT pg_is_in_recovery()")
        .fetch_one(&mut *standby_conn)
        .await?;
    assert!(recovering);

    // Exercise mutable segments, full-page images, Generic WAL, aborted inserts,
    // updates, deletes, COPY, VACUUM, immutable segments, and index rebuilds.
    for batch in 0..4 {
        sqlx::query("INSERT INTO items (description, category) SELECT 'replicated shoes ' || i, 'batch' FROM generate_series($1::int, $1::int + 127) s(i)")
            .bind(batch * 128).execute(&mut *primary_conn).await?;
    }
    primary_conn
        .execute("BEGIN; INSERT INTO items (description) VALUES ('rolled back shoes'); ROLLBACK")
        .await?;
    primary_conn
        .execute("UPDATE items SET description = 'updated headphones' WHERE id = 3")
        .await?;
    primary_conn
        .execute("DELETE FROM items WHERE id = 4 OR id % 7 = 0")
        .await?;
    let mut copy = primary_conn
        .copy_in_raw("COPY items(description, category) FROM STDIN")
        .await?;
    copy.send("copied shoes\tcopy\ncopied headphones\tcopy\n".as_bytes())
        .await?;
    copy.finish().await?;
    primary_conn.execute("VACUUM items").await?;
    primary_conn
        .execute("ALTER INDEX items_search_idx SET (mutable_segment_rows = 0)")
        .await?;
    for _ in 0..4 {
        primary_conn.execute("INSERT INTO items (description, category) SELECT 'immutable shoes ' || i, 'immutable' FROM generate_series(1, 128) s(i)").await?;
    }
    primary_conn.execute("VACUUM items").await?;
    // Rebuild a separate index so REINDEX cannot hide missing incremental WAL
    // for items_search_idx. The promoted items index is never rebuilt in this test.
    primary_conn
        .execute("CREATE TABLE rebuilt_items (id bigint, body text)")
        .await?;
    primary_conn
        .execute("INSERT INTO rebuilt_items VALUES (1, 'rebuilt shoes')")
        .await?;
    primary_conn
        .execute("CREATE INDEX rebuilt_items_idx ON rebuilt_items USING paradedb (id, body)")
        .await?;
    primary_conn
        .execute("REINDEX INDEX CONCURRENTLY rebuilt_items_idx")
        .await?;
    primary_conn
        .execute("INSERT INTO rebuilt_items VALUES (2, 'after rebuild shoes')")
        .await?;
    primary_conn.execute("CHECKPOINT").await?;
    wait_for_replay(primary_conn, standby_conn).await?;

    let expected: Vec<(i32, String)> =
        sqlx::query_as("SELECT id, description FROM items ORDER BY id")
            .fetch_all(&mut *primary_conn)
            .await?;
    let actual: Vec<(i32, String)> =
        sqlx::query_as("SELECT id, description FROM items ORDER BY id")
            .fetch_all(&mut *standby_conn)
            .await?;
    assert_eq!(
        actual, expected,
        "heap WAL must still replay after index maintenance"
    );

    // A standby restart must replay to consistency with the exact same extension.
    let ctl = &standby.pg_ctl_path;
    let data = &standby.tempdir_path;
    let log = &standby.log_path;
    run_cmd!($ctl -D $data -l $log restart -m fast)?;
    *standby_conn = standby.connection().await?;
    primary_conn
        .execute("INSERT INTO items (description) VALUES ('before failover shoes')")
        .await?;
    primary_conn
        .execute("UPDATE ordinary SET value = 'still replicating' WHERE id = 1")
        .await?;
    wait_for_replay(primary_conn, standby_conn).await?;

    let expected_all: Vec<(i32, String)> =
        sqlx::query_as("SELECT id, description FROM items ORDER BY id")
            .fetch_all(&mut *primary_conn)
            .await?;
    let expected_shoes: Vec<(i32,)> =
        sqlx::query_as("SELECT id FROM items WHERE description ||| 'shoes' ORDER BY id")
            .fetch_all(&mut *primary_conn)
            .await?;

    // Replay index changes belonging to a transaction that will never commit.
    // Promotion must preserve committed rows and never expose these documents.
    let mut pending = primary.connection().await?;
    pending.execute("BEGIN").await?;
    pending
        .execute("INSERT INTO items (description) VALUES ('uncommitted shoes')")
        .await?;
    pending
        .execute("UPDATE items SET description = 'uncommitted replacement' WHERE id = 1")
        .await?;
    pending.execute("DELETE FROM items WHERE id = 3").await?;
    primary_conn.execute("CHECKPOINT").await?;
    wait_for_replay(primary_conn, standby_conn).await?;

    // Simulate the database-level operation Patroni performs. No REINDEX on the
    // promoted node is permitted: these assertions must use the copied index.
    let ctl = &primary.pg_ctl_path;
    let data = &primary.tempdir_path;
    run_cmd!($ctl -D $data stop -m immediate)?;
    let ctl = &standby.pg_ctl_path;
    let data = &standby.tempdir_path;
    run_cmd!($ctl -D $data promote -w -t 60)?;

    let recovering: bool = sqlx::query_scalar("SELECT pg_is_in_recovery()")
        .fetch_one(&mut *standby_conn)
        .await?;
    assert!(!recovering);
    let actual_all: Vec<(i32, String)> =
        sqlx::query_as("SELECT id, description FROM items WHERE id @@@ pdb.all() ORDER BY id")
            .fetch_all(&mut *standby_conn)
            .await?;
    assert_eq!(
        actual_all, expected_all,
        "copied index must cover the heap after promotion"
    );
    let actual_shoes: Vec<(i32,)> =
        sqlx::query_as("SELECT id FROM items WHERE description ||| 'shoes' ORDER BY id")
            .fetch_all(&mut *standby_conn)
            .await?;
    assert_eq!(actual_shoes, expected_shoes);
    let empty_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM empty_items WHERE body ||| 'shoes'")
            .fetch_one(&mut *standby_conn)
            .await?;
    assert_eq!(empty_count, 0);
    let rebuilt_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM rebuilt_items WHERE body ||| 'shoes'")
            .fetch_one(&mut *standby_conn)
            .await?;
    assert_eq!(rebuilt_count, 2);

    standby_conn
        .execute("INSERT INTO items (description) VALUES ('promoted shoes')")
        .await?;
    standby_conn
        .execute("UPDATE items SET description = 'promoted headphones' WHERE id = 3")
        .await?;
    standby_conn
        .execute("DELETE FROM items WHERE id = 2")
        .await?;
    standby_conn.execute("VACUUM items").await?;
    let promoted: i64 =
        sqlx::query_scalar("SELECT count(*) FROM items WHERE description ||| 'promoted'")
            .fetch_one(&mut *standby_conn)
            .await?;
    assert_eq!(promoted, 2);
    let passed: Option<bool> = sqlx::query_scalar(
        "SELECT bool_and(passed) FROM pdb.verify_index('items_search_idx', heapallindexed => true)",
    )
    .fetch_one(&mut *standby_conn)
    .await?;
    assert_eq!(
        passed,
        Some(true),
        "copied index must verify after promoted writes and VACUUM"
    );
    Ok(())
}

// A background merger commits with synchronous_commit = local, so a synchronous standby that is
// missing cannot park it at commit holding its merge slot. With such a standby configured, the
// merge must still publish, every merger must exit on its own, and none may wait in SyncRep.
//
// Every test in this file needs PG_CONFIG for an ephemeral server; the main CI job skips them
// by name with `--skip replication --skip ephemeral`, so keep one of those words in the name.
#[async_std::test]
async fn test_background_merge_commits_without_synchronous_replication_standby() -> Result<()> {
    let primary = EphemeralPostgres::new(
        Some(
            "shared_preload_libraries = 'pg_search'\nsynchronous_standby_names = 'missing_standby'",
        ),
        None,
    );
    let mut conn = primary.connection().await?;
    conn.execute("SET synchronous_commit = off").await?;
    conn.execute("CREATE EXTENSION pg_search CASCADE").await?;
    conn.execute("CREATE TABLE merge_die (id bigint, body text)")
        .await?;
    // Seal segments during each insert instead of letting this small fixture remain
    // in mutable segments, which would not fill the configured background layers.
    conn.execute(
        "CREATE INDEX merge_die_idx ON merge_die USING paradedb (id, body) \
         WITH (layer_sizes = '0', background_layer_sizes = '0', target_segment_count = 1, mutable_segment_rows = 1)",
    )
    .await?;
    for _ in 0..12 {
        conn.execute(
            "INSERT INTO merge_die SELECT i, 'beer wine cheese ' || i FROM generate_series(1, 1000) s(i)",
        ).await?;
    }
    let segments = "SELECT count(*) FROM paradedb.index_info('merge_die_idx') WHERE visible";
    let (before,): (i64,) = sqlx::query_as(segments).fetch_one(&mut conn).await?;
    conn.execute("ALTER INDEX merge_die_idx SET (background_layer_sizes = '1kb,10kb,64kb')")
        .await?;
    conn.execute(
        "INSERT INTO merge_die SELECT i, 'beer wine cheese ' || i FROM generate_series(1, 1000) s(i)",
    ).await?;

    let result: Result<()> = async {
        // The merged segment becomes visible at the merger's local commit, which happens before
        // any wait for a standby, so a falling segment count proves the merge published; the
        // check below then proves the worker did not stay parked waiting for the standby.
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let waiting: Vec<(i32,)> = sqlx::query_as(
                "SELECT pid FROM pg_stat_activity WHERE backend_type LIKE 'background merger%'
                 AND wait_event = 'SyncRep'",
            )
            .fetch_all(&mut conn)
            .await?;
            anyhow::ensure!(
                waiting.is_empty(),
                "merge worker {} waited for the missing synchronous standby",
                waiting[0].0
            );
            let (after,): (i64,) = sqlx::query_as(segments).fetch_one(&mut conn).await?;
            if after < before {
                break;
            }
            anyhow::ensure!(
                Instant::now() < deadline,
                "no background merge published within the deadline ({before} segments)"
            );
            async_std::task::sleep(Duration::from_millis(50)).await;
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let alive: Vec<(i32, Option<String>)> = sqlx::query_as(
                "SELECT pid, wait_event FROM pg_stat_activity
                 WHERE backend_type LIKE 'background merger%'",
            )
            .fetch_all(&mut conn)
            .await?;
            if alive.is_empty() {
                break;
            }
            anyhow::ensure!(
                Instant::now() < deadline,
                "merge worker(s) still running after publishing: {alive:?}"
            );
            async_std::task::sleep(Duration::from_millis(50)).await;
        }
        let passed: Option<bool> = sqlx::query_scalar(
            "SELECT bool_and(passed) FROM pdb.verify_index('merge_die_idx', heapallindexed => true)",
        )
        .fetch_one(&mut conn)
        .await?;
        anyhow::ensure!(passed == Some(true), "verify_index failed after the merge");
        Ok(())
    }
    .await;

    // Let `Drop` stop the server even if a worker is parked.
    conn.execute("ALTER SYSTEM SET synchronous_standby_names = ''")
        .await?;
    conn.execute("SELECT pg_reload_conf()").await?;
    result
}

// A background merger works through all of its candidates in one transaction, so it must release
// the buffer pins of each segment it merges. Otherwise a merge over many small segments pins all
// of shared_buffers and fails with "no unpinned buffers available" (paradedb/paradedb#6796).
#[async_std::test]
async fn test_ephemeral_background_merge_releases_merged_segment_pins() -> Result<()> {
    // 2048 buffers: without the release, the merger fills them after about 125 small segments.
    let postgres = EphemeralPostgres::new(
        Some("shared_preload_libraries = 'pg_search'\nshared_buffers = '16MB'\nautovacuum = off"),
        None,
    );
    let mut conn = postgres.connection().await?;
    conn.execute("CREATE EXTENSION pg_search CASCADE").await?;
    conn.execute("CREATE TABLE merge_pins (id bigint, body text)")
        .await?;
    // Each insert seals its own segment, and nothing merges them until the background layers
    // are set below.
    conn.execute(
        "CREATE INDEX merge_pins_idx ON merge_pins USING paradedb (id, body) \
         WITH (background_layer_sizes = '0', target_segment_count = 1, mutable_segment_rows = 0)",
    )
    .await?;
    for id in 0..500_i64 {
        sqlx::query("INSERT INTO merge_pins VALUES ($1, 'beer wine cheese ' || $1)")
            .bind(id)
            .execute(&mut conn)
            .await?;
    }
    let segments = "SELECT count(*) FROM paradedb.index_info('merge_pins_idx')";
    let (before,): (i64,) = sqlx::query_as(segments).fetch_one(&mut conn).await?;
    conn.execute("ALTER INDEX merge_pins_idx SET (background_layer_sizes = '1kb,10kb,64kb')")
        .await?;
    // There are no mutable segments, so no backpressure: this insert launches the merger.
    conn.execute("INSERT INTO merge_pins VALUES (500, 'beer wine cheese 500')")
        .await?;

    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let (after,): (i64,) = sqlx::query_as(segments).fetch_one(&mut conn).await?;
        if after < before / 10 {
            break;
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "the background merge stopped at {after} of {before} segments"
        );
        async_std::task::sleep(Duration::from_millis(50)).await;
    }
    let passed: Option<bool> = sqlx::query_scalar(
        "SELECT bool_and(passed) FROM pdb.verify_index('merge_pins_idx', heapallindexed => true)",
    )
    .fetch_one(&mut conn)
    .await?;
    anyhow::ensure!(passed == Some(true), "verify_index failed after the merge");
    Ok(())
}

// With more than 2 mutable segments an insert is under backpressure and merges them in the
// foreground. With no foreground layers configured that merge leaves the immutable segments
// alone, so the insert must still launch the background merger, or concurrent writers keep it
// from ever running (paradedb/paradedb#6796).
#[async_std::test]
async fn test_ephemeral_insert_under_backpressure_launches_background_merger() -> Result<()> {
    let postgres = EphemeralPostgres::new(
        Some("shared_preload_libraries = 'pg_search'\nautovacuum = off"),
        None,
    );
    let mut conn = postgres.connection().await?;
    conn.execute("CREATE EXTENSION pg_search CASCADE").await?;
    conn.execute("CREATE TABLE merge_launch (id bigint, body text)")
        .await?;
    conn.execute(
        "CREATE INDEX merge_launch_idx ON merge_launch USING paradedb (id, body) \
         WITH (background_layer_sizes = '0', target_segment_count = 1)",
    )
    .await?;
    // Immutable segments, which only the background merger merges.
    conn.execute("SET paradedb.global_mutable_segment_rows = 0")
        .await?;
    for _ in 0..12 {
        conn.execute(
            "INSERT INTO merge_launch SELECT i, 'beer wine cheese ' || i FROM generate_series(1, 1000) s(i)",
        )
        .await?;
    }
    // Single-row inserts alternately create a mutable segment and fill it, which leaves two
    // frozen mutable segments.
    conn.execute("SET paradedb.global_mutable_segment_rows = 1")
        .await?;
    for id in 0..4_i64 {
        sqlx::query("INSERT INTO merge_launch VALUES ($1, 'single row')")
            .bind(id)
            .execute(&mut conn)
            .await?;
    }
    let immutable =
        "SELECT count(*) FROM paradedb.index_info('merge_launch_idx') WHERE NOT mutable";
    let (before,): (i64,) = sqlx::query_as(immutable).fetch_one(&mut conn).await?;
    conn.execute("ALTER INDEX merge_launch_idx SET (background_layer_sizes = '1kb,10kb,64kb')")
        .await?;
    // A third mutable segment puts this insert under backpressure.
    conn.execute("INSERT INTO merge_launch VALUES (4, 'single row')")
        .await?;

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let (after,): (i64,) = sqlx::query_as(immutable).fetch_one(&mut conn).await?;
        if after < before {
            break;
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "no background merge of the {before} immutable segments within the deadline"
        );
        async_std::task::sleep(Duration::from_millis(50)).await;
    }
    let passed: Option<bool> = sqlx::query_scalar(
        "SELECT bool_and(passed) FROM pdb.verify_index('merge_launch_idx', heapallindexed => true)",
    )
    .fetch_one(&mut conn)
    .await?;
    anyhow::ensure!(passed == Some(true), "verify_index failed after the merge");
    Ok(())
}
