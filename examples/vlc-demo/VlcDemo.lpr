program VlcDemo;

{$mode objfpc}{$H+}

// A video and audio player on the VLC library installed on this Mac (/Applications/VLC.app).
// Before building: File > Import Component Package... > Add Pascoda VLC, so the VLC components
// are on the palette and the units are found.

uses
  Interfaces, Forms, unit1;

begin
  Application.Initialize;
  Application.CreateForm(TForm1, Form1);
  Application.Run;
end.
