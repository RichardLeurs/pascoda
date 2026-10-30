# Pascoda data package

`PascodaData.lpk`: the database components on the Data Controls page of the palette: `TSQLConnector`,
`TSQLTransaction`, `TSQLQuery`, `TSQLScript`, `TDataSource` and `TSQLite3Connection`. Their units come
from the FPC database sources in `vendor/fpcsrc/packages/fcl-db` (no Lazarus IDE is needed). Import
it with File > Import Component Package... (Add Pascoda Data).

The icons (`tsqlconnector.png`, `tsqltransaction.png`, `tsqlquery.png`, `tsqlscript.png`,
`tdatasource.png`, `tsqlite3connection.png`) are Lazarus's component icons from `components/sqldb/design`
and `images/components`, under the modified LGPL (`COPYING.modifiedLGPL.txt`), like the VLC package's.
