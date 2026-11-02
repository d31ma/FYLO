// fylo link conformance for Turso's .NET provider: the stock packages, with
// the app's resolver loading the turso_sdk_kit library fylo link placed,
// open a FYLO root as their database.
// Usage: dotnet run -- <fylo-root> <plain-db-path> <turso_sdk_kit library | stock>
using System.Reflection;
using System.Runtime.InteropServices;
using Turso.Data.Sqlite;

var results = new List<string>();
var (root, plain, library) = (args[0], args[1], args[2]);
if (library != "stock")
{
    NativeLibrary.SetDllImportResolver(Assembly.Load("Turso.Raw"), (name, _, _) =>
        name == "turso_sdk_kit" ? NativeLibrary.Load(library) : IntPtr.Zero);
}

void Check(string name, Action body)
{
    try { body(); results.Add("PASS " + name); }
    catch (Exception error) { results.Add($"FAIL {name}: {error.GetType().Name}: {error.Message}"); }
}

void Expect(bool condition, string message)
{
    if (!condition) throw new InvalidOperationException(message);
}

void FailsWith(string code, Action body)
{
    try { body(); }
    catch (Exception error)
    {
        Expect(error.ToString().Contains(code), $"expected {code}, got {error.Message}");
        return;
    }
    throw new InvalidOperationException($"expected {code}, got success");
}

SqliteCommand Command(SqliteConnection connection, string sql, params object[] values)
{
    var command = connection.CreateCommand();
    command.CommandText = sql;
    for (var i = 0; i < values.Length; i++) command.Parameters.AddWithValue($"@p{i}", values[i]);
    return command;
}

int Update(SqliteConnection connection, string sql, params object[] values)
{
    using var command = Command(connection, sql, values);
    return command.ExecuteNonQuery();
}

List<string> Column(SqliteConnection connection, string sql, params object[] values)
{
    using var command = Command(connection, sql, values);
    using var reader = command.ExecuteReader();
    var found = new List<string>();
    while (reader.Read()) found.Add(Convert.ToString(reader.GetValue(0))!);
    return found;
}

string One(SqliteConnection connection, string sql, params object[] values)
{
    var found = Column(connection, sql, values);
    Expect(found.Count > 0, "no row for " + sql);
    return found[0];
}

var db = new SqliteConnection($"Data Source={root}");
db.Open();

Check("collections are tables", () =>
{
    Expect(One(db, "SELECT count(*) FROM products") == "60", "count");
    One(db, "SELECT name FROM products ORDER BY name LIMIT 1");
});
Check("pushdown returns what a full scan returns", () =>
{
    foreach (var (pushed, scanned, value) in new (string, string, object)[] { ("price = @p0", "price + 0 = @p0", 7), ("price > @p0", "price + 0 > @p0", 15), ("name = @p0", "name || '' = @p0", "item-0042") })
    {
        var a = Column(db, $"SELECT id FROM products WHERE {pushed} ORDER BY id", value);
        var b = Column(db, $"SELECT id FROM products WHERE {scanned} ORDER BY id", value);
        Expect(a.SequenceEqual(b) && a.Count > 0, $"{pushed}: {string.Join(",", a)} vs {string.Join(",", b)}");
    }
});
Check("insert, update and delete commit through FYLO", () =>
{
    Expect(Update(db, "INSERT INTO notes (doc) VALUES (@p0)", "{\"text\":\"hello\",\"n\":1}") == 1, "insert count");
    var inserted = One(db, "SELECT id FROM notes");
    Expect(One(db, "SELECT json_extract(doc, '$.text') FROM notes WHERE id = @p0", inserted) == "hello", "doc");
    Update(db, "UPDATE notes SET doc = json_set(doc, '$.n', 2) WHERE id = @p0", inserted);
    Expect(One(db, "SELECT json_extract(doc, '$.n') FROM notes WHERE id = @p0", inserted) == "2", "n");
    Expect(Update(db, "DELETE FROM products WHERE name = @p0", "item-0002") == 1, "delete count");
});
Check("a transaction commits whole and rolls back whole", () =>
{
    using (var transaction = db.BeginTransaction())
    {
        Update(db, "INSERT INTO notes (doc) VALUES (@p0), (@p1)", "{\"t\":1}", "{\"t\":2}");
        transaction.Commit();
    }
    using (var transaction = db.BeginTransaction())
    {
        Update(db, "DELETE FROM notes");
        transaction.Rollback();
    }
    Expect(One(db, "SELECT count(*) FROM notes") == "3", "notes");
});
Check("a second collection in one transaction is EFYLO_TXN_SCOPE", () =>
{
    using (var transaction = db.BeginTransaction())
    {
        Update(db, "INSERT INTO notes (doc) VALUES (@p0)", "{\"scope\":1}");
        FailsWith("EFYLO_TXN_SCOPE", () => Update(db, "INSERT INTO products (doc) VALUES ('{\"name\":\"x\"}')"));
        try { transaction.Rollback(); }
        catch (SqliteException) { } // the failed statement already ended the transaction
    }
    Expect(One(db, "SELECT count(*) FROM notes") == "3", "the transaction's write survived");
});
Check("FYLO errors keep their codes", () =>
    FailsWith("EARRAYOFOBJECTS", () => Update(db, "INSERT INTO notes (doc) VALUES (@p0)", "{\"tags\":[{\"a\":1}]}")));
Check("the queue is tables", () =>
{
    Update(db, "INSERT INTO fylo_queue_publish (topic, payload) VALUES ('jobs', @p0)", "{\"job\":1}");
    string id, receipt;
    using (var command = Command(db, "SELECT id, receipt FROM fylo_queue_claim WHERE topic = 'jobs' AND group_name = 'workers'"))
    using (var reader = command.ExecuteReader())
    {
        Expect(reader.Read(), "no claim");
        id = reader.GetString(0);
        receipt = reader.GetString(1);
    }
    Update(db, "INSERT INTO fylo_queue_ack (topic, group_name, id, receipt) VALUES ('jobs', 'workers', @p0, @p1)", id, receipt);
    Expect(One(db, "SELECT value FROM fylo_info WHERE name = 'turso_core'") == "0.8.3", "version");
});
db.Close();
Check("a plain database path opens as stock Turso", () =>
{
    using var file = new SqliteConnection($"Data Source={plain}");
    file.Open();
    Update(file, "CREATE TABLE t (x INTEGER)");
    Update(file, "INSERT INTO t VALUES (1), (2)");
    Expect(One(file, "SELECT sum(x) FROM t") == "3", "total");
    file.Close();
    Expect(File.Exists(plain), "no database file");
});

Console.WriteLine(string.Join("\n", results));
return results.Any(line => line.StartsWith("FAIL")) ? 1 : 0;
