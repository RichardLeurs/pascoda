program DbDemo;

{$mode objfpc}{$H+}

// A window on a SQLite database (ideas.sqlite3, next to the program): search the ideas with a
// parameterised query and see them in a grid. The database is opened read-only in spirit: the demo
// only reads it.
//
// The SQLdb components are on the designer's palette (Data Controls page); no package is needed.
// A project that uses them gets FPC's database units (fcl-db) on its build path by itself.

uses
  Interfaces, Forms, unit1;

begin
  Application.Initialize;
  Application.CreateForm(TForm1, Form1);
  Application.Run;
end.
