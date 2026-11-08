// fylo link conformance for tech.turso:turso (JDBC): the stock driver, with
// FYLO's _turso_java on java.library.path, opens a FYLO root as its database.
// Usage: java -cp turso-0.8.3.jar -Djava.library.path=<dir> Conformance.java <fylo-root> <plain-db-path>
import java.nio.file.Files;
import java.nio.file.Path;
import java.sql.Connection;
import java.sql.DriverManager;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
import java.sql.SQLException;
import java.sql.Statement;
import java.util.ArrayList;
import java.util.List;

public class Conformance {
    interface Body { void run() throws Exception; }

    static final List<String> results = new ArrayList<>();

    static void check(String name, Body body) {
        try {
            body.run();
            results.add("PASS " + name);
        } catch (Throwable error) {
            results.add("FAIL " + name + ": " + error);
        }
    }

    static void expect(boolean condition, String message) {
        if (!condition) throw new AssertionError(message);
    }

    static void failsWith(String code, Body body) throws Exception {
        try {
            body.run();
        } catch (SQLException error) {
            expect(String.valueOf(error.getMessage()).contains(code), "expected " + code + ", got " + error);
            return;
        }
        throw new AssertionError("expected " + code + ", got success");
    }

    static PreparedStatement prepare(Connection db, String sql, Object... args) throws SQLException {
        PreparedStatement statement = db.prepareStatement(sql);
        for (int i = 0; i < args.length; i++) statement.setObject(i + 1, args[i]);
        return statement;
    }

    static int update(Connection db, String sql, Object... args) throws SQLException {
        try (PreparedStatement statement = prepare(db, sql, args)) {
            return statement.executeUpdate();
        }
    }

    static List<String> column(Connection db, String sql, Object... args) throws SQLException {
        List<String> values = new ArrayList<>();
        try (PreparedStatement statement = prepare(db, sql, args); ResultSet rows = statement.executeQuery()) {
            while (rows.next()) values.add(String.valueOf(rows.getObject(1)));
        }
        return values;
    }

    static String one(Connection db, String sql, Object... args) throws SQLException {
        List<String> values = column(db, sql, args);
        expect(!values.isEmpty(), "no row for " + sql);
        return values.get(0);
    }

    static void execute(Connection db, String sql) throws SQLException {
        try (Statement statement = db.createStatement()) {
            statement.execute(sql);
        }
    }

    public static void main(String[] args) throws Exception {
        String root = args[0], plain = args[1];
        Connection db = DriverManager.getConnection("jdbc:turso:" + root);

        check("collections are tables", () -> {
            expect(one(db, "SELECT count(*) FROM products").equals("60"), "count");
            one(db, "SELECT name FROM products ORDER BY name LIMIT 1");
        });
        check("pushdown returns what a full scan returns", () -> {
            Object[][] cases = {{"price = ?", "price + 0 = ?", 7}, {"price > ?", "price + 0 > ?", 15}, {"name = ?", "name || '' = ?", "item-0042"}};
            for (Object[] c : cases) {
                List<String> a = column(db, "SELECT id FROM products WHERE " + c[0] + " ORDER BY id", c[2]);
                List<String> b = column(db, "SELECT id FROM products WHERE " + c[1] + " ORDER BY id", c[2]);
                expect(a.equals(b) && !a.isEmpty(), c[0] + ": " + a + " vs " + b);
            }
        });
        check("insert, update and delete commit through FYLO", () -> {
            expect(update(db, "INSERT INTO notes (doc) VALUES (?)", "{\"text\":\"hello\",\"n\":1}") == 1, "insert count");
            String inserted = one(db, "SELECT id FROM notes");
            expect(one(db, "SELECT json_extract(doc, '$.text') FROM notes WHERE id = ?", inserted).equals("hello"), "doc");
            update(db, "UPDATE notes SET doc = json_set(doc, '$.n', 2) WHERE id = ?", inserted);
            expect(one(db, "SELECT json_extract(doc, '$.n') FROM notes WHERE id = ?", inserted).equals("2"), "n");
            expect(update(db, "DELETE FROM products WHERE name = ?", "item-0002") == 1, "delete count");
        });
        check("a transaction commits whole and rolls back whole", () -> {
            db.setAutoCommit(false);
            update(db, "INSERT INTO notes (doc) VALUES (?), (?)", "{\"t\":1}", "{\"t\":2}");
            db.commit();
            update(db, "DELETE FROM notes");
            db.rollback();
            db.setAutoCommit(true);
            expect(one(db, "SELECT count(*) FROM notes").equals("3"), "notes");
        });
        check("a second collection in one transaction is EFYLO_TXN_SCOPE", () -> {
            db.setAutoCommit(false);
            update(db, "INSERT INTO notes (doc) VALUES (?)", "{\"scope\":1}");
            failsWith("EFYLO_TXN_SCOPE", () -> update(db, "INSERT INTO products (doc) VALUES ('{\"name\":\"x\"}')"));
            try {
                db.rollback();
            } catch (SQLException alreadyEnded) {
                // the failed statement already ended the transaction
            }
            try {
                db.setAutoCommit(true);
            } catch (SQLException alreadyEnded) {
                // setAutoCommit commits a transaction the driver still thinks is open
            }
            expect(one(db, "SELECT count(*) FROM notes").equals("3"), "the transaction's write survived");
        });
        check("FYLO errors keep their codes", () ->
            failsWith("EARRAYOFOBJECTS", () -> update(db, "INSERT INTO notes (doc) VALUES (?)", "{\"tags\":[{\"a\":1}]}")));
        check("the queue is tables", () -> {
            update(db, "INSERT INTO fylo_queue_publish (topic, payload) VALUES ('jobs', ?)", "{\"job\":1}");
            String id, receipt;
            try (PreparedStatement statement = prepare(db, "SELECT id, receipt FROM fylo_queue_claim WHERE topic = 'jobs' AND group_name = 'workers'");
                 ResultSet rows = statement.executeQuery()) {
                expect(rows.next(), "no claim");
                id = rows.getString(1);
                receipt = rows.getString(2);
            }
            update(db, "INSERT INTO fylo_queue_ack (topic, group_name, id, receipt) VALUES ('jobs', 'workers', ?, ?)", id, receipt);
            expect(one(db, "SELECT value FROM fylo_info WHERE name = 'turso_core'").equals("0.8.3"), "version");
        });
        db.close();
        check("a plain database path opens as stock Turso", () -> {
            try (Connection file = DriverManager.getConnection("jdbc:turso:" + plain)) {
                execute(file, "CREATE TABLE t (x INTEGER)");
                execute(file, "INSERT INTO t VALUES (1), (2)");
                expect(one(file, "SELECT sum(x) FROM t").equals("3"), "total");
            }
            expect(Files.isRegularFile(Path.of(plain)), "no database file");
        });

        System.out.println(String.join("\n", results));
        System.exit(results.stream().anyMatch(line -> line.startsWith("FAIL")) ? 1 : 0);
    }
}
