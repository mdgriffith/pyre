# Pyre 0.1.19

- Fix reloading stored schemas containing newline-separated permission conditions after enum values, including nested relational permissions. Preserve newline separators when parsing enum values without payloads so existing affected schemas can be read from the database.
