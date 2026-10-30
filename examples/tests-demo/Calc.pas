unit Calc;

{$mode objfpc}{$H+}

// The code under test. CalcTests.pas holds the tests; the Tests pane finds them because that unit
// mentions TTestCase.

interface

function Add(A, B: Integer): Integer;
function Divide(A, B: Integer): Integer;

implementation

uses
  SysUtils;

function Add(A, B: Integer): Integer;
begin
  Result := A + B;
end;

function Divide(A, B: Integer): Integer;
begin
  if B = 0 then
    raise EDivByZero.Create('Divide: the divisor is zero');
  Result := A div B;
end;

end.
