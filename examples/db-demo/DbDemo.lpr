program DbDemo;

{$mode objfpc}{$H+}

// A window on a SQLite database (ideas.sqlite3, next to the program): search the ideas with a
// parameterised query and see them in a grid. The database is opened read-only in spirit: the demo
// only reads it.
//
// Build & Run needs the Pascoda data package imported (File > Import Component Package... > Add
// PascodaData): it provides the SQLdb components on the palette.

uses
  Interfaces, Forms, unit1;

begin
  Application.Initialize;
  Application.CreateForm(TForm1, Form1);
  Application.Run;
end.
