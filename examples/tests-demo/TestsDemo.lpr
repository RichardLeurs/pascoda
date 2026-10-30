program TestsDemo;

{$mode objfpc}{$H+}

// Runs the tests in this folder with FPCUnit's console runner: Build & Run prints each test and the
// result in the Output pane. The Tests pane (Cmd+U) runs the same tests with its own runner.

uses
  consoletestrunner, CalcTests;

var
  App: TTestRunner;
begin
  DefaultFormat := fPlain;
  DefaultRunAllTests := True;
  App := TTestRunner.Create(nil);
  App.Initialize;
  App.Title := 'Calc tests';
  App.Run;
  App.Free;
end.
