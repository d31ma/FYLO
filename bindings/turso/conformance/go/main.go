// fylo link conformance for tursogo: a stock SDK, loading the FYLO-built
// library through LoadStrategy "system", opens a FYLO root as its database.
// Usage: go run . <fylo-root> <plain-db-path>
package main

import (
	"database/sql"
	"encoding/json"
	"fmt"
	"os"
	"strings"

	turso_libs "github.com/tursodatabase/turso-go-platform-libs"
	turso "turso.tech/database/tursogo"
)

var results []string

func check(name string, body func() error) {
	if err := body(); err != nil {
		results = append(results, fmt.Sprintf("FAIL %s: %v", name, err))
		return
	}
	results = append(results, "PASS "+name)
}

func failsWith(code string, err error) error {
	if err == nil {
		return fmt.Errorf("expected %s, got success", code)
	}
	if !strings.Contains(err.Error(), code) {
		return fmt.Errorf("expected %s, got %v", code, err)
	}
	return nil
}

func ids(db *sql.DB, query string, args ...any) ([]string, error) {
	rows, err := db.Query(query, args...)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []string
	for rows.Next() {
		var id string
		if err := rows.Scan(&id); err != nil {
			return nil, err
		}
		out = append(out, id)
	}
	return out, rows.Err()
}

func main() {
	root, plain := os.Args[1], os.Args[2]
	turso.InitLibrary(turso_libs.LoadTursoLibraryConfig{LoadStrategy: "system"})
	db, err := sql.Open("turso", root)
	if err != nil {
		panic(err)
	}
	db.SetMaxOpenConns(1)

	check("collections are tables", func() error {
		var count int
		if err := db.QueryRow("SELECT count(*) FROM products").Scan(&count); err != nil {
			return err
		}
		if count != 60 {
			return fmt.Errorf("count %d", count)
		}
		var name string
		return db.QueryRow("SELECT name FROM products ORDER BY name LIMIT 1").Scan(&name)
	})

	check("pushdown returns what a full scan returns", func() error {
		for _, c := range [][3]any{{"price = ?", "price + 0 = ?", 7}, {"price > ?", "price + 0 > ?", 15}, {"name = ?", "name || '' = ?", "item-0042"}} {
			a, err := ids(db, "SELECT id FROM products WHERE "+c[0].(string)+" ORDER BY id", c[2])
			if err != nil {
				return err
			}
			b, err := ids(db, "SELECT id FROM products WHERE "+c[1].(string)+" ORDER BY id", c[2])
			if err != nil {
				return err
			}
			if strings.Join(a, ",") != strings.Join(b, ",") || len(a) == 0 {
				return fmt.Errorf("%s: %v vs %v", c[0], a, b)
			}
		}
		return nil
	})

	var inserted string
	check("insert, update and delete commit through FYLO", func() error {
		res, err := db.Exec("INSERT INTO notes (doc) VALUES (?)", `{"text":"hello","n":1}`)
		if err != nil {
			return err
		}
		if n, _ := res.RowsAffected(); n != 1 {
			return fmt.Errorf("insert changed %d rows", n)
		}
		var doc string
		if err := db.QueryRow("SELECT id, doc FROM notes").Scan(&inserted, &doc); err != nil {
			return err
		}
		var fields map[string]any
		if err := json.Unmarshal([]byte(doc), &fields); err != nil || fields["text"] != "hello" {
			return fmt.Errorf("doc %s", doc)
		}
		if _, err := db.Exec("UPDATE notes SET doc = json_set(doc, '$.n', 2) WHERE id = ?", inserted); err != nil {
			return err
		}
		var n int
		if err := db.QueryRow("SELECT json_extract(doc, '$.n') FROM notes WHERE id = ?", inserted).Scan(&n); err != nil || n != 2 {
			return fmt.Errorf("n %d %v", n, err)
		}
		res, err = db.Exec("DELETE FROM products WHERE name = ?", "item-0002")
		if err != nil {
			return err
		}
		if n, _ := res.RowsAffected(); n != 1 {
			return fmt.Errorf("delete changed %d rows", n)
		}
		return nil
	})

	check("a transaction commits whole and rolls back whole", func() error {
		tx, err := db.Begin()
		if err != nil {
			return err
		}
		if _, err := tx.Exec("INSERT INTO notes (doc) VALUES (?), (?)", `{"t":1}`, `{"t":2}`); err != nil {
			return err
		}
		if err := tx.Commit(); err != nil {
			return err
		}
		tx, err = db.Begin()
		if err != nil {
			return err
		}
		if _, err := tx.Exec("DELETE FROM notes"); err != nil {
			return err
		}
		if err := tx.Rollback(); err != nil {
			return err
		}
		var count int
		if err := db.QueryRow("SELECT count(*) FROM notes").Scan(&count); err != nil || count != 3 {
			return fmt.Errorf("notes %d %v", count, err)
		}
		return nil
	})

	check("a second collection in one transaction is EFYLO_TXN_SCOPE", func() error {
		tx, err := db.Begin()
		if err != nil {
			return err
		}
		defer tx.Rollback()
		if _, err := tx.Exec("INSERT INTO notes (doc) VALUES (?)", `{"scope":1}`); err != nil {
			return err
		}
		_, err = tx.Exec(`INSERT INTO products (doc) VALUES ('{"name":"x"}')`)
		return failsWith("EFYLO_TXN_SCOPE", err)
	})

	check("FYLO errors keep their codes", func() error {
		_, err := db.Exec("INSERT INTO notes (doc) VALUES (?)", `{"tags":[{"a":1}]}`)
		return failsWith("EARRAYOFOBJECTS", err)
	})

	check("the queue is tables", func() error {
		if _, err := db.Exec("INSERT INTO fylo_queue_publish (topic, payload) VALUES ('jobs', ?)", `{"job":1}`); err != nil {
			return err
		}
		var id, receipt, payload string
		var attempt int
		if err := db.QueryRow("SELECT id, receipt, attempt, payload FROM fylo_queue_claim WHERE topic = 'jobs' AND group_name = 'workers'").Scan(&id, &receipt, &attempt, &payload); err != nil {
			return err
		}
		if _, err := db.Exec("INSERT INTO fylo_queue_ack (topic, group_name, id, receipt) VALUES ('jobs', 'workers', ?, ?)", id, receipt); err != nil {
			return err
		}
		var value string
		if err := db.QueryRow("SELECT value FROM fylo_info WHERE name = 'turso_core'").Scan(&value); err != nil || value != "0.8.3" {
			return fmt.Errorf("version %q %v", value, err)
		}
		return nil
	})
	db.Close()

	check("a plain database path opens as stock Turso", func() error {
		file, err := sql.Open("turso", plain)
		if err != nil {
			return err
		}
		defer file.Close()
		if _, err := file.Exec("CREATE TABLE t (x INTEGER)"); err != nil {
			return err
		}
		if _, err := file.Exec("INSERT INTO t VALUES (1), (2)"); err != nil {
			return err
		}
		var total int
		if err := file.QueryRow("SELECT sum(x) FROM t").Scan(&total); err != nil || total != 3 {
			return fmt.Errorf("total %d %v", total, err)
		}
		if _, err := os.Stat(plain); err != nil {
			return err
		}
		return nil
	})

	fmt.Println(strings.Join(results, "\n"))
	for _, line := range results {
		if strings.HasPrefix(line, "FAIL") {
			os.Exit(1)
		}
	}
}
