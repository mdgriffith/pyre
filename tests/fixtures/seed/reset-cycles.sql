BEGIN;
PRAGMA defer_foreign_keys = ON;

-- Dropping cycle_b cascades into cycle_c, which still references cycle_a.
CREATE TABLE cycle_a (id INTEGER PRIMARY KEY, c INTEGER REFERENCES cycle_c(id));
CREATE TABLE cycle_b (id INTEGER PRIMARY KEY, a INTEGER REFERENCES cycle_a(id));
CREATE TABLE cycle_c (id INTEGER PRIMARY KEY, a INTEGER REFERENCES cycle_a(id), b INTEGER REFERENCES cycle_b(id) ON DELETE CASCADE);
INSERT INTO cycle_a VALUES (1, 1);
INSERT INTO cycle_b VALUES (1, 1);
INSERT INTO cycle_c VALUES (1, 1, 1);

-- Either drop order triggers an immediate NOT NULL error with FKs enabled.
CREATE TABLE cycle_x (id INTEGER PRIMARY KEY, y INTEGER NOT NULL REFERENCES cycle_y(id) ON DELETE SET NULL);
CREATE TABLE cycle_y (id INTEGER PRIMARY KEY, x INTEGER NOT NULL REFERENCES cycle_x(id) ON DELETE SET NULL);
INSERT INTO cycle_x VALUES (1, 1);
INSERT INTO cycle_y VALUES (1, 1);

CREATE TABLE self_ref (id INTEGER PRIMARY KEY, parent INTEGER REFERENCES self_ref(id) ON DELETE RESTRICT);
INSERT INTO self_ref VALUES (1, NULL), (2, 1);

-- Real user tables must never supply the reset planner's PRAGMA metadata.
CREATE TABLE pragma_foreign_keys (foreign_keys INTEGER);
INSERT INTO pragma_foreign_keys VALUES (0);
CREATE TABLE pragma_database_list (name TEXT);
CREATE TABLE pragma_table_info (name TEXT);
CREATE TABLE pragma_table_list (schema TEXT, name TEXT, type TEXT);
INSERT INTO pragma_table_list VALUES ('main', 'pragma_table_list', 'shadow'), ('main', 'self_ref', 'shadow');
COMMIT;
