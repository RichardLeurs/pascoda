unit CalcTests;

{$mode objfpc}{$H+}

// Tests for the Calc unit, written for FPCUnit. Each published method is one test.
// Press Cmd+U (or Run All in the Tests pane) to build and run them. Double-click a test to run
// only it. TestAddIsCommutative fails on purpose, so the Tests pane shows a failure and "Go to test".

interface

uses
  SysUtils, fpcunit, testregistry, Calc;

type
  TCalcTests = class(TTestCase)
  published
    procedure TestAddsTwoNumbers;
    procedure TestAddIsCommutative;
    procedure TestDivides;
    procedure TestDivideByZeroRaises;
  end;

implementation

procedure TCalcTests.TestAddsTwoNumbers;
begin
  AssertEquals('2 + 3', 5, Add(2, 3));
end;

procedure TCalcTests.TestAddIsCommutative;
begin
  // Fails on purpose: the expected value here is wrong.
  AssertEquals('3 + 4 and 4 + 3', 8, Add(3, 4));
end;

procedure TCalcTests.TestDivides;
begin
  AssertEquals('12 / 4', 3, Divide(12, 4));
end;

procedure TCalcTests.TestDivideByZeroRaises;
begin
  try
    Divide(1, 0);
    Fail('Divide by zero should raise');
  except
    on E: EDivByZero do
      ; // expected
  end;
end;

initialization
  RegisterTest(TCalcTests);

end.
