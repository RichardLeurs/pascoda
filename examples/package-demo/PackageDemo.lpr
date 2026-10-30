program PackageDemo;

{$mode objfpc}{$H+}

// Uses the TextTools unit from the package in this folder.

uses
  TextTools;

begin
  WriteLn(Capitalise('hELLO pascal'));
  WriteLn(Reversed('pascoda'));
end.
