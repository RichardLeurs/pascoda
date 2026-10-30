unit RegPascodaData;

{$mode objfpc}{$H+}

// Puts the database components on the palette, on the Data page: connections, transactions,
// queries, scripts and the data source that connects a dataset to the controls that show it.

interface

procedure Register;

implementation

uses
  Classes, db, sqldb, sqlite3conn;

procedure Register;
begin
  RegisterComponents('Data Controls', [TSQLConnector, TSQLTransaction, TSQLQuery, TSQLScript, TDataSource, TSQLite3Connection]);
end;

end.
