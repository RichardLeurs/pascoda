unit TextTools;

{$mode objfpc}{$H+}

// The unit inside the TextTools package (TextTools.lpk). Any program in this project finds it
// through the package, as Lazarus does.

interface

function Capitalise(const S: string): string;
function Reversed(const S: string): string;

implementation

function Capitalise(const S: string): string;
begin
  Result := LowerCase(S);
  if Result <> '' then
    Result[1] := UpCase(Result[1]);
end;

function Reversed(const S: string): string;
var
  i: Integer;
begin
  Result := '';
  for i := Length(S) downto 1 do
    Result := Result + S[i];
end;

end.
