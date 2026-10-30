program MathsDemo;

{$mode objfpc}{$H+}

// The program that uses the Maths unit next to it. Build & Run builds both.

uses
  Maths;

var
  n: Integer;
begin
  WriteLn('10! = ', Factorial(10));
  for n := 1 to 20 do
    if IsPrime(n) then
      Write(n, ' ');
  WriteLn;
end.
