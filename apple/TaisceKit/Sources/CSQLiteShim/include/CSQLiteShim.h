#ifndef CSQLITESHIM_H
#define CSQLITESHIM_H

#include <sqlite3.h>

/// `sqlite3_db_config(db, SQLITE_DBCONFIG_DEFENSIVE, on, NULL)`: the C
/// function is variadic, which Swift can't call.
int taisce_sqlite3_set_defensive(sqlite3 *db, int on);

#endif
