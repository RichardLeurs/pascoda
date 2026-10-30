unit Maths;

{$mode objfpc}{$H+}

// A unit other programs use: the routines are declared in the interface and written below it.

interface

function Factorial(N: Integer): Int64;
function IsPrime(N: Integer): Boolean;

implementation

function Factorial(N: Integer): Int64;
var
  i: Integer;
begin
  Result := 1;
  for i := 2 to N do
    Result := Result * i;
end;

function IsPrime(N: Integer): Boolean;
var
  d: Integer;
begin
  if N < 2 then
    Exit(False);
  for d := 2 to N div 2 do
    if N mod d = 0 then
      Exit(False);
  Result := True;
end;

end.
