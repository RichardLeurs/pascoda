program WordCount;

{$mode objfpc}{$H+}

// Reads text from standard input and reports its lines, words and characters, as the Unix wc does.
// Build & Run, then in the Terminal pane: ./WordCount < some.txt   (or type lines and press Ctrl+D)

uses
  SysUtils;

function CountWords(const Line: string): Integer;
var
  i: Integer;
  inWord: Boolean;
begin
  Result := 0;
  inWord := False;
  for i := 1 to Length(Line) do
    if Line[i] in [' ', #9] then
      inWord := False
    else
    begin
      if not inWord then
        Inc(Result);
      inWord := True;
    end;
end;

var
  Line: string;
  Lines, Words, Chars: Integer;
begin
  Lines := 0;
  Words := 0;
  Chars := 0;
  while not EOF(Input) do
  begin
    ReadLn(Line);
    Inc(Lines);
    Inc(Words, CountWords(Line));
    Inc(Chars, Length(Line) + 1);
  end;
  WriteLn(Format('%d lines, %d words, %d characters', [Lines, Words, Chars]));
end.
