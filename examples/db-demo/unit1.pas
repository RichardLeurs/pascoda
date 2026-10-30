unit Unit1;

{$mode objfpc}{$H+}

interface

uses
  Classes, SysUtils, Forms, Controls, Graphics, Dialogs, StdCtrls, DBGrids, DB, sqldb, sqlite3conn;

type
  TForm1 = class(TForm)
    FilterEdit: TEdit;
    SearchButton: TButton;
    DBGrid1: TDBGrid;
    StatusLabel: TLabel;
    SQLConnector1: TSQLConnector;
    SQLTransaction1: TSQLTransaction;
    SQLQuery1: TSQLQuery;
    DataSource1: TDataSource;
    procedure FormCreate(Sender: TObject);
    procedure SearchButtonClick(Sender: TObject);
    procedure DataSource1Click(Sender: TObject);
  private
    procedure Search(const Pattern: string);
  public
  end;

var
  Form1: TForm1;

implementation

{$R *.lfm}

// The database sits next to the program. A program run from its .app bundle (the IDE's Build & Run
// wraps GUI programs in one) keeps the database beside the bundle, not in Contents/MacOS.
function DatabasePath: string;
var
  Folder: string;
begin
  Folder := ExtractFilePath(ParamStr(0));
  if Pos('.app/Contents/MacOS/', Folder) > 0 then
    Folder := ExpandFileName(Folder + '../../..') + '/';
  Result := Folder + 'ideas.sqlite3';
end;

procedure TForm1.FormCreate(Sender: TObject);
begin
  SQLConnector1.DatabaseName := DatabasePath;
  SQLConnector1.Transaction := SQLTransaction1;
  SQLQuery1.DataBase := SQLConnector1;
  SQLQuery1.Transaction := SQLTransaction1;
  // The pattern is a parameter: text typed in the box is data, never SQL. The text is cast to
  // VARCHAR: SQLdb turns a declared TEXT column into a memo field, which the grid shows as "(Memo)".
  SQLQuery1.SQL.Text :=
    'SELECT id, CAST(text AS VARCHAR(500)) AS text, done FROM ideas WHERE text LIKE :pat ORDER BY id';
  Search('');
  // Used by the tests: print what the query returns and leave.
  if GetEnvironmentVariable('DBDEMO_SELFTEST') = '1' then
  begin
    WriteLn('STATUS ', StatusLabel.Caption);
    WriteLn('ROWS ', SQLQuery1.RecordCount);
    WriteLn('TEXT ', SQLQuery1.FieldByName('text').DataType);
    Flush(Output);
    Halt(0);
  end;
end;

procedure TForm1.Search(const Pattern: string);
var
  I: Integer;
begin
  try
    SQLQuery1.Close;
    SQLQuery1.Params.ParamByName('pat').AsString := '%' + Pattern + '%';
    SQLQuery1.Open;
    // Column widths can only be set once the grid has built its columns from the open query.
    for I := 0 to DBGrid1.Columns.Count - 1 do
      case DBGrid1.Columns[I].FieldName of
        'id': DBGrid1.Columns[I].Width := 60;
        'text': DBGrid1.Columns[I].Width := 400;
        'done': DBGrid1.Columns[I].Width := 60;
      end;
    StatusLabel.Caption := IntToStr(SQLQuery1.RecordCount) + ' ideas';
  except
    // Shown in the window: a dialog would wait for someone to press OK.
    on E: Exception do
      StatusLabel.Caption := 'Could not read the database: ' + E.Message;
  end;
end;

procedure TForm1.SearchButtonClick(Sender: TObject);
begin
  Search(FilterEdit.Text);
end;


procedure TForm1.DataSource1Click(Sender: TObject);
begin
end;
end.
