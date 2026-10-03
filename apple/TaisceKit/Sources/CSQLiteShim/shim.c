#include "CSQLiteShim.h"

int taisce_sqlite3_set_defensive(sqlite3 *db, int on) {
    int now = -1;
    int rc = sqlite3_db_config(db, SQLITE_DBCONFIG_DEFENSIVE, on, &now);
    return rc == SQLITE_OK && now == on ? SQLITE_OK : (rc == SQLITE_OK ? SQLITE_ERROR : rc);
}
